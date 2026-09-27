use super::client::{
    page_error, AdapterError, AdapterOutput, ClientFuture, GenerateRequest, PageClient, SiteAdapter,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TestModel {
    pub id: &'static str,
    #[serde(rename = "imagePolicy")]
    pub image_policy: &'static str,
    #[serde(rename = "type")]
    pub model_type: &'static str,
    pub url: &'static str,
}

pub const TEST_MODELS: &[TestModel] = &[
    TestModel {
        id: "cloudflare-turnstile",
        image_policy: "forbidden",
        model_type: "image",
        url: "https://nopecha.com/captcha/turnstile",
    },
    TestModel {
        id: "creepjs",
        image_policy: "forbidden",
        model_type: "image",
        url: "https://abrahamjuliot.github.io/creepjs/",
    },
    TestModel {
        id: "antibot",
        image_policy: "forbidden",
        model_type: "image",
        url: "https://bot.sannysoft.com/",
    },
    TestModel {
        id: "browserleaks-js",
        image_policy: "forbidden",
        model_type: "image",
        url: "https://browserleaks.com/javascript",
    },
    TestModel {
        id: "browserleaks-ip",
        image_policy: "forbidden",
        model_type: "image",
        url: "https://browserleaks.com/ip",
    },
    TestModel {
        id: "ip",
        image_policy: "forbidden",
        model_type: "text",
        url: "https://api.ip.sb/ip",
    },
    TestModel {
        id: "webgl",
        image_policy: "forbidden",
        model_type: "image",
        url: "https://get.webgl.org/",
    },
    TestModel {
        id: "ping0",
        image_policy: "forbidden",
        model_type: "image",
        url: "https://ping0.cc/",
    },
];

/// Debug adapter workflows, including the iframe and closed-shadow Turnstile
/// click path. The browser runtime exposes only generic DOM/frame primitives;
/// all challenge decisions remain in this Rust adapter.
#[derive(Debug, Default, Clone, Copy)]
pub struct TestAdapter;

impl TestAdapter {
    pub fn workflow_supported(model_id: &str) -> bool {
        TEST_MODELS.iter().any(|model| model.id == model_id)
    }
}

impl SiteAdapter for TestAdapter {
    fn id(&self) -> &'static str {
        "test"
    }
    fn target_url(&self) -> &'static str {
        "https://abrahamjuliot.github.io/creepjs/"
    }
    fn model_ids(&self) -> &'static [&'static str] {
        &[
            "cloudflare-turnstile",
            "creepjs",
            "antibot",
            "browserleaks-js",
            "browserleaks-ip",
            "ip",
            "webgl",
            "ping0",
        ]
    }

    fn generate<'a>(
        &'a self,
        page: &'a dyn PageClient,
        request: &'a GenerateRequest,
    ) -> ClientFuture<'a, AdapterOutput> {
        Box::pin(async move {
            let Some(model) = TEST_MODELS
                .iter()
                .find(|model| model.id == request.model_id)
            else {
                return Ok(AdapterOutput::error(format!(
                    "未找到模型配置: {}",
                    request.model_id
                )));
            };
            match generate(page, model).await {
                Ok(output) => Ok(output),
                Err(error) => Ok(AdapterOutput::normalized_error(error)),
            }
        })
    }
}

async fn generate(page: &dyn PageClient, model: &TestModel) -> Result<AdapterOutput, AdapterError> {
    let response = page
        .call(vec![
            json!({"op":"goto", "url":model.url, "options":{"waitUntil":"load", "timeout":120000}}),
        ])
        .await
        .map_err(|error| page_error(&error.message).unwrap_or(error))?;
    if let Some(status) = response
        .first()
        .and_then(|v| v.get("status"))
        .and_then(Value::as_u64)
    {
        if status >= 400 {
            return Err(AdapterError::normalized(
                format!("网站无法访问 (HTTP {status})"),
                "HTTP_ERROR",
                status >= 500,
            ));
        }
    }
    if model.id == "cloudflare-turnstile" {
        page.call(vec![json!({"op":"waitForTimeout", "ms":3500})])
            .await?;
        click_turnstile(page, "#example-container5", 3000).await?;
        return screenshot(page, model.id).await;
    }
    let delay_ms = if model.id == "ping0" {
        2500
    } else if model.model_type == "text" {
        1500
    } else {
        4000
    };
    page.call(vec![json!({"op":"waitForTimeout", "ms":delay_ms})])
        .await?;
    if model.model_type == "text" {
        let values = page
            .call(vec![
                json!({"op":"evaluate", "expression":"document.bodyText"}),
            ])
            .await?;
        let text = values
            .first()
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_owned();
        return Ok(AdapterOutput::text(text));
    }
    if model.id == "ping0" {
        let exists = page
            .call(vec![json!({"op":"evaluate", "expression":"element.exists", "args":{"selector":"#captcha-element"}})])
            .await?
            .first()
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if exists {
            let _ = click_turnstile(page, "#captcha-element", 5000).await;
            page.call(vec![json!({"op":"waitForTimeout", "ms":4000})])
                .await?;
        }
    }
    screenshot(page, model.id).await
}

async fn screenshot(page: &dyn PageClient, model_id: &str) -> Result<AdapterOutput, AdapterError> {
    let path = screenshot_path(model_id);
    page.call(vec![
        json!({"op":"screenshot", "path":path, "options":{"type":"png", "fullPage":true}}),
    ])
    .await?;
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|error| AdapterError::new(format!("读取截图失败: {error}")))?;
    let _ = tokio::fs::remove_file(&path).await;
    Ok(AdapterOutput::image(format!(
        "data:image/png;base64,{}",
        STANDARD.encode(bytes)
    )))
}

/// Generic `shadow.query` returns opaque element handles. Rust chooses which
/// host/frame/query to inspect and whether to fall back to coordinate clicks.
async fn click_turnstile(
    page: &dyn PageClient,
    host_selector: &str,
    wait_after_click_ms: u64,
) -> Result<(), AdapterError> {
    let host = page
        .call(vec![json!({
            "op":"shadow.query",
            "root":{"kind":"page"},
            "hostSelector":host_selector,
            "shadowProperty":"shadowRootUnl",
            "selector":"iframe",
            "includeRoot":false
        })])
        .await?
        .into_iter()
        .next()
        .filter(|value| !value.is_null())
        .ok_or_else(|| AdapterError::new("未找到有 shadowRootUnl 的 iframe"))?;
    let element_id = host
        .get("elementId")
        .and_then(Value::as_str)
        .ok_or_else(|| AdapterError::new("shadow.query 未返回 iframe elementId"))?;
    let bounding_box = host
        .get("boundingBox")
        .cloned()
        .filter(|value| !value.is_null());

    let frame = page
        .call(vec![
            json!({"op":"frame.fromElement", "elementId":element_id}),
        ])
        .await?
        .into_iter()
        .next()
        .unwrap_or(Value::Null);
    let frame_id = frame.get("frameId").and_then(Value::as_str);
    if let Some(frame_id) = frame_id {
        page.call(vec![json!({"op":"waitForTimeout", "ms":1500})])
            .await?;
        let mut checkbox = Value::Null;
        for selector in ["input[type='checkbox']", "input"] {
            checkbox = page
                .call(vec![json!({
                    "op":"shadow.query",
                    "root":{"kind":"frame", "frameId":frame_id},
                    "shadowProperty":"shadowRootUnl",
                    "selector":selector,
                    "includeRoot":true
                })])
                .await?
                .into_iter()
                .next()
                .unwrap_or(Value::Null);
            if !checkbox.is_null() {
                break;
            }
        }
        if let Some(checkbox_id) = checkbox.get("elementId").and_then(Value::as_str) {
            if page
                .call(vec![
                    json!({"op":"frame.click", "frameId":frame_id, "elementId":checkbox_id}),
                ])
                .await
                .is_ok()
            {
                page.call(vec![
                    json!({"op":"waitForTimeout", "ms":wait_after_click_ms + 1500}),
                ])
                .await?;
                return Ok(());
            }
        }
    }

    // The legacy helper clicks at x + 28, centered vertically in the iframe
    // when a frame handle or shadow checkbox cannot be reached.
    let Some(rect) = bounding_box else {
        return Err(AdapterError::new(
            "未找到 Turnstile checkbox，且 iframe 无 boundingBox",
        ));
    };
    let x = rect.get("x").and_then(Value::as_f64).unwrap_or(0.0) + 28.0;
    let y = rect.get("y").and_then(Value::as_f64).unwrap_or(0.0)
        + rect.get("height").and_then(Value::as_f64).unwrap_or(0.0) / 2.0;
    page.call(vec![
        json!({"op":"mouse.move", "x":x, "y":y, "options":{"steps":10}}),
    ])
    .await?;
    page.call(vec![json!({"op":"waitForTimeout", "ms":400})])
        .await?;
    page.call(vec![json!({"op":"mouse.click", "x":x, "y":y})])
        .await?;
    page.call(vec![
        json!({"op":"waitForTimeout", "ms":wait_after_click_ms + 1500}),
    ])
    .await?;
    Ok(())
}

fn screenshot_path(model_id: &str) -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "webai2api-test-adapter-{}-{}-{model_id}.png",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::client::mock::MockPage;
    use serde_json::json;

    #[test]
    fn metadata_matches_legacy_test_manifest() {
        let adapter = TestAdapter;
        assert_eq!(adapter.id(), "test");
        assert_eq!(
            adapter.target_url(),
            "https://abrahamjuliot.github.io/creepjs/"
        );
        assert_eq!(adapter.model_ids().len(), 8);
        assert_eq!(TEST_MODELS.len(), 8);
        assert!(TestAdapter::workflow_supported("cloudflare-turnstile"));
        assert!(TestAdapter::workflow_supported("ping0"));
        assert!(TestAdapter::workflow_supported("creepjs"));
        assert_eq!(
            TEST_MODELS
                .iter()
                .find(|model| model.id == "ip")
                .unwrap()
                .model_type,
            "text"
        );
    }

    #[tokio::test]
    async fn ip_model_navigates_and_returns_page_text() {
        let page = MockPage::returning([
            json!({"url":"https://api.ip.sb/ip", "status":200}),
            Value::Null,
            json!("  203.0.113.4\n"),
        ]);
        let request = GenerateRequest {
            prompt: "ignored".into(),
            model_id: "ip".into(),
            image_paths: vec![],
            reasoning: false,
            wait_timeout_ms: 1000,
            adapter_config: Value::Null,
        };
        let output = TestAdapter.generate(&page, &request).await.unwrap();
        assert_eq!(output.text.as_deref(), Some("203.0.113.4"));
        let calls = page.calls.lock().unwrap();
        assert_eq!(calls[0][0]["op"], "goto");
        assert_eq!(calls[0][0]["url"], "https://api.ip.sb/ip");
        assert_eq!(calls[2][0]["expression"], "document.bodyText");
    }

    #[tokio::test]
    async fn turnstile_uses_generic_shadow_and_frame_operations() {
        let page = MockPage::returning([
            json!({"url":"https://nopecha.com/captcha/turnstile", "status":200}),
            Value::Null,
            json!({"elementId":"iframe-element", "boundingBox":{"x":10,"y":20,"width":300,"height":70}}),
            json!({"frameId":"challenge-frame"}),
            Value::Null,
            json!({"elementId":"checkbox-element", "boundingBox":null}),
            Value::Null,
            Value::Null,
            json!({"path":"mock"}),
        ]);
        let request = GenerateRequest {
            prompt: "ignored".into(),
            model_id: "cloudflare-turnstile".into(),
            image_paths: vec![],
            reasoning: false,
            wait_timeout_ms: 1000,
            adapter_config: Value::Null,
        };
        let output = TestAdapter.generate(&page, &request).await.unwrap();
        assert!(output.error.is_none());
        assert!(output
            .image
            .as_deref()
            .unwrap()
            .starts_with("data:image/png;base64,"));
        let operations: Vec<Value> = page
            .calls
            .lock()
            .unwrap()
            .iter()
            .flatten()
            .cloned()
            .collect();
        assert!(operations
            .iter()
            .any(|op| op["op"] == "shadow.query" && op["hostSelector"] == "#example-container5"));
        assert!(operations.iter().any(|op| op["op"] == "frame.fromElement"));
        assert!(operations
            .iter()
            .any(|op| op["op"] == "frame.click" && op["elementId"] == "checkbox-element"));
    }

    #[tokio::test]
    async fn ping0_processes_captcha_when_present_then_screenshots() {
        let page = MockPage::returning([
            json!({"url":"https://ping0.cc/", "status":200}),
            Value::Null,
            json!(true),
            json!({"elementId":"iframe-element", "boundingBox":{"x":10,"y":20,"width":300,"height":70}}),
            json!({"frameId":"challenge-frame"}),
            Value::Null,
            json!({"elementId":"checkbox-element", "boundingBox":null}),
            Value::Null,
            Value::Null,
            Value::Null,
            json!({"path":"mock"}),
        ]);
        let request = GenerateRequest {
            prompt: "ignored".into(),
            model_id: "ping0".into(),
            image_paths: vec![],
            reasoning: false,
            wait_timeout_ms: 1000,
            adapter_config: Value::Null,
        };
        let output = TestAdapter.generate(&page, &request).await.unwrap();
        assert!(output.error.is_none());
        let operations: Vec<Value> = page
            .calls
            .lock()
            .unwrap()
            .iter()
            .flatten()
            .cloned()
            .collect();
        assert!(operations
            .iter()
            .any(|op| op["op"] == "evaluate" && op["args"]["selector"] == "#captcha-element"));
        assert!(operations.iter().any(|op| op["op"] == "frame.click"));
        assert!(operations.iter().any(|op| op["op"] == "screenshot"));
    }
}
