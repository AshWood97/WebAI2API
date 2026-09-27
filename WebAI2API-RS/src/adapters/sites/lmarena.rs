use super::super::client::{
    http_error, page_error, AdapterError, AdapterOutput, ClientFuture, GenerateRequest, PageClient,
    SiteAdapter,
};
use base64::Engine;
use serde_json::{json, Value};
use std::{path::PathBuf, time::Duration};

const TARGET_URL: &str = "https://arena.ai/image/direct";

// Model IDs and picker aliases are generated from the legacy manifest.
const MODEL_META: &[(&str, Option<&str>, bool)] = &[
    (
        "gemini-3.1-flash-image-preview",
        Some("gemini-3.1-flash-image-preview (nano-banana-2) [web-search]"),
        false,
    ),
    ("gpt-image-1.5-high-fidelity", None, false),
    (
        "gemini-3-pro-image-preview-2k",
        Some("gemini-3-pro-image-preview-2k (nano-banana-pro)"),
        false,
    ),
    ("mai-image-2", None, false),
    ("reve-v1.5", None, false),
    ("flux-2-max", None, false),
    ("flux-2-flex", None, false),
    ("flux-2-pro", None, false),
    ("hunyuan-image-3.0", None, false),
    ("flux-2-dev", None, false),
    ("seedream-4.5", None, false),
    ("qwen-image-2512", None, false),
    ("imagen-4.0-generate-001", None, false),
    ("wan2.5-t2i-preview", None, false),
    ("gpt-image-1", None, false),
    ("seedream-5.0-lite", None, false),
    ("seedream-4-high-res-fal", None, false),
    ("gpt-image-1-mini", None, false),
    ("recraft-v4", None, false),
    ("seedream-3", None, false),
    ("flux-2-klein-9b", None, false),
    ("qwen-image-prompt-extend", None, false),
    ("flux-1-kontext-pro", None, false),
    ("imagen-3.0-generate-002", None, false),
    ("ideogram-v3-quality", None, false),
    ("photon", None, false),
    ("p-image", None, false),
    ("flux-2-klein-4b", None, false),
    ("recraft-v3", None, false),
    ("runway-gen4", None, false),
    ("lucid-origin", None, false),
    ("dall-e-3", None, false),
    ("flux-1-kontext-dev", None, false),
    ("imagen-4.0-ultra-generate-001", None, false),
    ("p-image-edit", None, false),
    ("hunyuan-image-2.1", None, false),
    ("reve-v1.1", None, false),
    ("vidu-q2-image", None, false),
    ("imagen-4.0-fast-generate-001", None, false),
    ("qwen-image-2.0", None, false),
    ("qwen-image-2.0-pro", None, false),
    ("reve-v1.1-fast", None, false),
    ("kling-image-o1", None, false),
    (
        "chatgpt-image-latest-high-fidelity",
        Some("chatgpt-image-latest-high-fidelity (20251216)"),
        false,
    ),
    ("hunyuan-image-3.0-instruct", None, false),
    ("wan2.7-image", None, false),
    ("grok-imagine-image-pro", None, false),
    ("grok-imagine-image", None, false),
    ("wan2.7-image-pro", None, false),
    ("qwen-image-edit-2511", None, false),
    (
        "gemini-2.5-flash-image-preview",
        Some("gemini-2.5-flash-image-preview (nano-banana)"),
        false,
    ),
    ("wan2.5-i2i-preview", None, false),
    ("qwen-image-edit", None, false),
    ("wan2.6-image", None, false),
    ("seededit-3.0", None, false),
    ("wan2.6-t2i", None, false),
];
const MODEL_IDS: &[&str] = &[
    "gemini-3.1-flash-image-preview",
    "gpt-image-1.5-high-fidelity",
    "gemini-3-pro-image-preview-2k",
    "mai-image-2",
    "reve-v1.5",
    "flux-2-max",
    "flux-2-flex",
    "flux-2-pro",
    "hunyuan-image-3.0",
    "flux-2-dev",
    "seedream-4.5",
    "qwen-image-2512",
    "imagen-4.0-generate-001",
    "wan2.5-t2i-preview",
    "gpt-image-1",
    "seedream-5.0-lite",
    "seedream-4-high-res-fal",
    "gpt-image-1-mini",
    "recraft-v4",
    "seedream-3",
    "flux-2-klein-9b",
    "qwen-image-prompt-extend",
    "flux-1-kontext-pro",
    "imagen-3.0-generate-002",
    "ideogram-v3-quality",
    "photon",
    "p-image",
    "flux-2-klein-4b",
    "recraft-v3",
    "runway-gen4",
    "lucid-origin",
    "dall-e-3",
    "flux-1-kontext-dev",
    "imagen-4.0-ultra-generate-001",
    "p-image-edit",
    "hunyuan-image-2.1",
    "reve-v1.1",
    "vidu-q2-image",
    "imagen-4.0-fast-generate-001",
    "qwen-image-2.0",
    "qwen-image-2.0-pro",
    "reve-v1.1-fast",
    "kling-image-o1",
    "chatgpt-image-latest-high-fidelity",
    "hunyuan-image-3.0-instruct",
    "wan2.7-image",
    "grok-imagine-image-pro",
    "grok-imagine-image",
    "wan2.7-image-pro",
    "qwen-image-edit-2511",
    "gemini-2.5-flash-image-preview",
    "wan2.5-i2i-preview",
    "qwen-image-edit",
    "wan2.6-image",
    "seededit-3.0",
    "wan2.6-t2i",
];

pub struct LmArena;

impl SiteAdapter for LmArena {
    fn id(&self) -> &'static str {
        "lmarena"
    }
    fn target_url(&self) -> &'static str {
        TARGET_URL
    }
    fn model_ids(&self) -> &'static [&'static str] {
        MODEL_IDS
    }

    fn generate<'a>(
        &'a self,
        page: &'a dyn PageClient,
        request: &'a GenerateRequest,
    ) -> ClientFuture<'a, AdapterOutput> {
        Box::pin(async move {
            match generate(page, request).await {
                Ok(output) => Ok(output),
                Err(error) => Ok(AdapterOutput::normalized_error(
                    page_error(&error.message).unwrap_or(error),
                )),
            }
        })
    }
}

async fn generate(
    page: &dyn PageClient,
    request: &GenerateRequest,
) -> Result<AdapterOutput, AdapterError> {
    const TEXTAREA: &str = "textarea";
    let goto = page
        .call(vec![
            json!({"op":"goto", "url":TARGET_URL, "options":{"waitUntil":"load", "timeout":30000}}),
        ])
        .await?;
    if let Some(status) = goto
        .first()
        .and_then(|v| v.get("status"))
        .and_then(Value::as_u64)
    {
        if status >= 400 {
            return Err(AdapterError::new(format!("网站无法访问 (HTTP {status})")));
        }
    }
    page.call(vec![json!({"op":"locator.waitFor", "locator":{"kind":"css","value":TEXTAREA}, "options":{"state":"visible","timeout":120000}}),
                  json!({"op":"locator.click", "locator":{"kind":"css","value":TEXTAREA}})]).await?;

    if !request.model_id.is_empty() {
        // The original UI exposes its model picker two Shift+Tab stops before
        // the prompt. Keep the same keyboard workflow and manifest alias.
        let search = MODEL_META
            .iter()
            .find(|(id, _, _)| *id == request.model_id)
            .and_then(|(_, alias, _)| *alias)
            .unwrap_or(&request.model_id);
        page.call(vec![
            json!({"op":"keyboard.press","key":"Shift+Tab"}),
            json!({"op":"keyboard.press","key":"Shift+Tab"}),
            json!({"op":"keyboard.press","key":"Enter"}),
            json!({"op":"waitForTimeout","ms":150}),
            json!({"op":"keyboard.type","text":search}),
        ])
        .await?;
        // Matching the selected option stays on the Rust side; timing out is
        // tolerated for the same reason as the legacy picker flow.
        let filtered = page.call(vec![json!({"op":"locator.waitFor","locator":{"kind":"css","value":"[role=option]","chain":[{"op":"first"}]},"options":{"state":"visible","timeout":5000}}),
                                     json!({"op":"locator.text","locator":{"kind":"css","value":"[role=option]","chain":[{"op":"first"}]},"options":{"timeout":1000}})]).await;
        if let Ok(values) = filtered {
            let _first_option_matches = values
                .get(1)
                .and_then(Value::as_str)
                .is_some_and(|text| text.contains(&request.model_id));
        }
        page.call(vec![
            json!({"op":"waitForTimeout","ms":400}),
            json!({"op":"keyboard.press","key":"Enter"}),
        ])
        .await?;
    }

    if !request.image_paths.is_empty() {
        let inputs = page
            .call(vec![
                json!({"op":"locator.count","locator":{"kind":"css","value":"input[type=file]"}}),
            ])
            .await?;
        let input_count = inputs.first().and_then(Value::as_u64).unwrap_or_default() as usize;
        if input_count == 0 {
            return Err(AdapterError::new(
                "未找到任何 input[type=file] 控件,无法上传",
            ));
        }
        let files: Vec<String> = request
            .image_paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect();
        let mut uploaded = false;
        let mut upload_error = None;
        for index in 0..input_count {
            let locator = json!({"kind":"css","value":"input[type=file]","chain":[{"op":"nth","index":index}]});
            let connected = page
                .call(vec![json!({"op":"locator.connected","locator":locator})])
                .await?;
            if !connected.first().and_then(Value::as_bool).unwrap_or(false) {
                continue;
            }
            match page
                .call(vec![json!({"op":"upload","locator":locator,"files":files})])
                .await
            {
                Ok(_) => {
                    uploaded = true;
                    break;
                }
                Err(error) => upload_error = Some(error),
            }
        }
        if !uploaded {
            return Err(
                upload_error.unwrap_or_else(|| AdapterError::new("所有文件控件均无法接受输入"))
            );
        }
        page.call(vec![json!({"op":"waitForTimeout","ms":750})])
            .await?;
    }

    page.call(vec![
        json!({"op":"locator.click","locator":{"kind":"css","value":TEXTAREA}}),
        json!({"op":"keyboard.type","text":request.prompt}),
    ])
    .await?;
    let mut after = page.subscribe_responses().await?;
    page.call(vec![
        json!({"op":"locator.click","locator":{"kind":"css","value":"button[type=submit]"}}),
    ])
    .await?;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(request.wait_timeout_ms);
    let response = loop {
        let mut matched = None;
        for event in page.poll_events(after).await? {
            after = after.max(event.sequence);
            if event.event_type == "response"
                && event.request_method.as_deref() == Some("POST")
                && event
                    .status
                    .is_some_and(|status| status == 200 || status >= 400)
                && event
                    .url
                    .as_deref()
                    .is_some_and(|url| url.contains("/nextjs-api/stream"))
            {
                matched = Some(event);
                break;
            }
        }
        if let Some(event) = matched {
            break event;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(AdapterError::normalized(
                format!(
                    "API_TIMEOUT: 等待响应超时 ({}秒)",
                    request.wait_timeout_ms / 1000
                ),
                "TIMEOUT_ERROR",
                true,
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let response_id = response
        .response_id
        .as_deref()
        .ok_or_else(|| AdapterError::new("响应事件缺少 responseId"))?;
    page.response_wait_finished(response_id, request.wait_timeout_ms)
        .await?;
    let body = page.response_body(response_id).await?;
    let body = String::from_utf8_lossy(&body);
    let status = response.status.unwrap_or(200);
    if status >= 400 {
        if let Some(error) = http_error(status, &body) {
            return Ok(AdapterOutput::normalized_error(AdapterError::normalized(
                format!("请求生成时返回错误: {}", error.message),
                error.code.unwrap_or_else(|| "HTTP_ERROR".into()),
                error.retryable.unwrap_or(false),
            )));
        }
    }
    if let Some(error) = extract_sse_error(&body) {
        return Ok(AdapterOutput::normalized_error(AdapterError::normalized(
            error,
            "UPSTREAM_ERROR",
            false,
        )));
    }
    let image_url = extract_image(&body).ok_or_else(|| {
        AdapterError::new(format!(
            "未获得结果，响应中无图片数据: {}",
            body.chars().take(200).collect::<String>()
        ))
    })?;
    if request
        .adapter_config
        .get("returnUrl")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(AdapterOutput::image(image_url));
    }
    download_image(page, &image_url, request).await
}

async fn download_image(
    page: &dyn PageClient,
    url: &str,
    request: &GenerateRequest,
) -> Result<AdapterOutput, AdapterError> {
    let retries = if request
        .adapter_config
        .get("imgDlRetry")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        request
            .adapter_config
            .get("imgDlRetryMaxRetries")
            .and_then(Value::as_u64)
            .unwrap_or(2)
            .min(8)
    } else {
        0
    };
    let mut last_error = None;
    for attempt in 0..=retries {
        let path: PathBuf =
            std::env::temp_dir().join(format!("webai2api-lmarena-{}.img", uuid_like()));
        match page
            .download_fetch_info(url, &path, json!({}), 120000)
            .await
        {
            Ok(download) => {
                let bytes = tokio::fs::read(&path).await.map_err(|error| {
                    AdapterError::new(format!("已获取结果，但图片下载时遇到错误: {error}"))
                })?;
                let _ = tokio::fs::remove_file(&path).await;
                let content_type = download
                    .content_type
                    .as_deref()
                    .unwrap_or("image/png")
                    .split(';')
                    .next()
                    .unwrap_or("image/png")
                    .trim();
                let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
                return Ok(AdapterOutput::image(format!(
                    "data:{content_type};base64,{encoded}"
                )));
            }
            Err(error) => {
                let _ = tokio::fs::remove_file(&path).await;
                last_error = Some(error);
                if attempt < retries {
                    tokio::time::sleep(Duration::from_millis(1000 * (attempt + 1))).await;
                }
            }
        }
    }
    Err(AdapterError::new(format!(
        "已获取结果，但图片下载时遇到错误: {}",
        last_error
            .map(|e| e.message)
            .unwrap_or_else(|| "下载失败".into())
    )))
}

fn uuid_like() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

fn extract_image(body: &str) -> Option<String> {
    body.lines().find_map(|line| {
        let data = line.strip_prefix("a2:")?;
        let value: Value = serde_json::from_str(data).ok()?;
        value.get(0)?.get("image")?.as_str().map(str::to_owned)
    })
}

fn extract_sse_error(body: &str) -> Option<String> {
    for line in body.lines() {
        if let Some(data) = line.strip_prefix("a3:") {
            if let Ok(Value::String(message)) = serde_json::from_str::<Value>(data) {
                if let Some(start) = message.find('{') {
                    if let Ok(value) = serde_json::from_str::<Value>(&message[start..]) {
                        if let Some(error) = value.get("error") {
                            if let Some(detail) = error.get("message").and_then(Value::as_str) {
                                let code = error
                                    .get("code")
                                    .and_then(Value::as_str)
                                    .unwrap_or("unknown");
                                return Some(format!("[模型错误] {detail} (code: {code})"));
                            }
                        }
                    }
                }
                return Some(format!("[模型错误] {message}"));
            }
        }
        if let Some(data) = line.strip_prefix("ae:") {
            if let Ok(value) = serde_json::from_str::<Value>(data) {
                if let Some(message) = value.get("message").and_then(Value::as_str) {
                    return Some(format!("[平台错误] {message}"));
                }
                if let Some(message) = value.as_str() {
                    return Some(format!("[平台错误] {message}"));
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::client::mock::MockPage;
    use std::sync::Arc;

    fn req() -> GenerateRequest {
        GenerateRequest {
            prompt: "sunset".into(),
            model_id: "gemini-3-pro-image-preview-2k".into(),
            image_paths: vec![],
            reasoning: false,
            wait_timeout_ms: 5,
            adapter_config: json!({"returnUrl":true}),
        }
    }

    #[test]
    fn manifest_contract_has_expected_image_models() {
        assert_eq!(LmArena.model_ids().len(), 56);
        assert_eq!(LmArena.id(), "lmarena");
        assert_eq!(LmArena.target_url(), TARGET_URL);
        assert_eq!(
            MODEL_META
                .iter()
                .find(|(id, _, _)| *id == "gemini-3-pro-image-preview-2k")
                .unwrap()
                .1,
            Some("gemini-3-pro-image-preview-2k (nano-banana-pro)")
        );
    }

    #[tokio::test]
    async fn rust_flow_navigates_selects_submits_and_extracts_image_url() {
        let page = Arc::new(MockPage::returning([
            json!({"url":TARGET_URL,"status":200}),
            json!(null),
            json!(null),
            json!(null),
            json!(null),
            json!(null),
            json!(null),
        ]));
        page.events
            .lock()
            .unwrap()
            .push(crate::adapters::client::BrowserEvent {
                sequence: 1,
                event_type: "response".into(),
                response_id: Some("resp-1".into()),
                url: Some("https://arena.ai/nextjs-api/stream".into()),
                status: Some(200),
                request_method: Some("POST".into()),
                ..Default::default()
            });
        *page.body.lock().unwrap() = "a2:[{\"image\":\"https://cdn.example/image.png\"}]\n".into();
        let request = req();
        let output = LmArena.generate(page.as_ref(), &request).await.unwrap();
        assert_eq!(
            output.image.as_deref(),
            Some("https://cdn.example/image.png")
        );
        let calls = page.calls.lock().unwrap();
        assert!(calls
            .iter()
            .flatten()
            .any(|op| op.get("url").and_then(Value::as_str) == Some(TARGET_URL)));
        assert!(calls
            .iter()
            .flatten()
            .any(|op| op.get("key").and_then(Value::as_str) == Some("Enter")));
        assert!(calls.iter().flatten().any(|op| op
            .get("locator")
            .and_then(|v| v.get("value"))
            .and_then(Value::as_str)
            == Some("button[type=submit]")));
    }

    #[tokio::test]
    async fn sse_provider_error_is_non_retryable() {
        let page =
            MockPage::returning([json!({"status":200}), json!(null), json!(null), json!(null)]);
        page.events
            .lock()
            .unwrap()
            .push(crate::adapters::client::BrowserEvent {
                sequence: 1,
                event_type: "response".into(),
                response_id: Some("resp-1".into()),
                url: Some("/nextjs-api/stream".into()),
                status: Some(200),
                request_method: Some("POST".into()),
                ..Default::default()
            });
        *page.body.lock().unwrap() = "a3:\"{\\\"error\\\":{\\\"message\\\":\\\"blocked prompt\\\",\\\"code\\\":\\\"policy\\\"}}\"".into();
        let output = LmArena.generate(&page, &req()).await.unwrap();
        assert!(output.error.unwrap().contains("blocked prompt"));
        assert_eq!(output.retryable, Some(false));
    }

    #[tokio::test]
    async fn image_upload_scans_connected_file_inputs() {
        let page = MockPage::returning([
            json!({"url":TARGET_URL,"status":200}),
            Value::Null,
            Value::Null,
            json!(2),
            json!(false),
            json!(true),
            Value::Null,
        ]);
        page.events
            .lock()
            .unwrap()
            .push(crate::adapters::client::BrowserEvent {
                sequence: 1,
                event_type: "response".into(),
                response_id: Some("resp-1".into()),
                url: Some("/nextjs-api/stream".into()),
                status: Some(200),
                request_method: Some("POST".into()),
                ..Default::default()
            });
        *page.body.lock().unwrap() = "a2:[{\"image\":\"https://cdn.example/image.png\"}]".into();
        let mut request = req();
        request.model_id.clear();
        request.image_paths.push(PathBuf::from("/tmp/input.png"));
        let output = LmArena.generate(&page, &request).await.unwrap();
        assert_eq!(
            output.image.as_deref(),
            Some("https://cdn.example/image.png")
        );
        let calls = page.calls.lock().unwrap();
        assert!(calls.iter().flatten().any(|op| {
            op.get("op").and_then(Value::as_str) == Some("locator.connected")
                && op
                    .get("locator")
                    .and_then(|l| l.get("chain"))
                    .and_then(Value::as_array)
                    .is_some_and(|chain| {
                        chain
                            .first()
                            .and_then(|step| step.get("index"))
                            .and_then(Value::as_u64)
                            == Some(1)
                    })
        }));
        assert!(calls.iter().flatten().any(|op| {
            op.get("op").and_then(Value::as_str) == Some("upload")
                && op
                    .get("files")
                    .and_then(Value::as_array)
                    .is_some_and(|files| {
                        files.first().and_then(Value::as_str) == Some("/tmp/input.png")
                    })
        }));
    }
}
