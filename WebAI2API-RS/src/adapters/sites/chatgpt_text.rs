//! ChatGPT text workflow, including model menu selection, file upload, SSE
//! parsing, DOM fallback, and the configured temporary-chat URL.

use crate::adapters::client::{
    chained, count_result, css, http_error, op, page_error, role, string_result, AdapterError,
    AdapterOutput, BrowserEvent, ClientFuture, GenerateRequest, PageClient, SiteAdapter,
};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

pub const CHATGPT_TEXT_TARGET_URL: &str = "https://chatgpt.com/";
const INPUT_SELECTOR: &str = ".ProseMirror";
pub const CHATGPT_TEXT_MODELS: &[(&str, &str)] = &[
    ("gpt-instant", "Instant"),
    ("gpt-thinking", "Thinking"),
    ("gpt-pro", "Pro"),
];

#[derive(Debug, Default, Clone, Copy)]
pub struct ChatGptTextAdapter;

impl SiteAdapter for ChatGptTextAdapter {
    fn id(&self) -> &'static str {
        "chatgpt_text"
    }
    fn target_url(&self) -> &'static str {
        CHATGPT_TEXT_TARGET_URL
    }
    fn model_ids(&self) -> &'static [&'static str] {
        &["gpt-instant", "gpt-thinking", "gpt-pro"]
    }

    fn generate<'a>(
        &'a self,
        page: &'a dyn PageClient,
        request: &'a GenerateRequest,
    ) -> ClientFuture<'a, AdapterOutput> {
        Box::pin(async move {
            if !self.model_ids().contains(&request.model_id.as_str()) {
                return Ok(AdapterOutput::error(format!(
                    "未找到模型配置: {}",
                    request.model_id
                )));
            }
            match generate(page, request).await {
                Ok(output) => Ok(output),
                Err(error) => Ok(AdapterOutput::normalized_error(error)),
            }
        })
    }
}

/// Accept both the Rust request's adapter-scoped settings and the original
/// full-config shape so existing callers can pass either representation.
pub fn temporary_chat(adapter_config: &Value) -> bool {
    adapter_config
        .get("temporaryChat")
        .and_then(Value::as_bool)
        .or_else(|| {
            adapter_config
                .pointer("/backend/adapter/chatgpt_text/temporaryChat")
                .and_then(Value::as_bool)
        })
        .unwrap_or(false)
}

pub fn target_url_for(adapter_config: &Value) -> &'static str {
    if temporary_chat(adapter_config) {
        "https://chatgpt.com/?temporary-chat=true"
    } else {
        CHATGPT_TEXT_TARGET_URL
    }
}

async fn call(page: &dyn PageClient, operations: Vec<Value>) -> Result<Vec<Value>, AdapterError> {
    page.call(operations).await
}

async fn wait_for_input(page: &dyn PageClient, timeout_ms: u64) -> Result<Value, AdapterError> {
    let locator = chained(css(INPUT_SELECTOR), "last", None);
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(timeout_ms) {
        if count_result(
            &call(page, vec![op("locator.count", locator.clone())]).await?,
            0,
        ) > 0
            && call(page, vec![op("locator.visible", locator.clone())])
                .await?
                .first()
                .and_then(Value::as_bool)
                .unwrap_or(false)
        {
            return Ok(locator);
        }
        call(page, vec![json!({"op":"waitForTimeout", "ms":500})]).await?;
    }
    Err(AdapterError::normalized(
        "未找到输入框 (.ProseMirror)",
        "TIMEOUT_ERROR",
        true,
    ))
}

async fn select_model(page: &dyn PageClient, code_name: &str) -> Result<bool, AdapterError> {
    let mut trigger = None;
    for selector in ["[data-testid='model-switcher-dropdown-button']", "button"] {
        let candidates = css(selector);
        let count = count_result(
            &call(page, vec![op("locator.count", candidates.clone())]).await?,
            0,
        );
        for index in 0..count {
            let locator = chained(candidates.clone(), "nth", Some(index));
            if selector.starts_with("[data-testid") {
                trigger = Some(locator);
                break;
            }
            let label = string_result(
                &call(page, vec![op("locator.text", locator.clone())]).await?,
                0,
            )
            .unwrap_or("")
            .to_ascii_lowercase();
            if [
                "chatgpt",
                "gpt",
                "instant",
                "thinking",
                "pro",
                "model selector",
                "models",
            ]
            .iter()
            .any(|needle| label.contains(needle))
            {
                trigger = Some(locator);
                break;
            }
        }
        if trigger.is_some() {
            break;
        }
    }
    let Some(trigger) = trigger else {
        return Ok(false);
    };
    call(page, vec![op("locator.click", trigger)]).await?;
    call(page, vec![json!({"op":"waitForTimeout", "ms":600})]).await?;
    let legacy = role("menuitem", "Legacy models");
    if count_result(
        &call(page, vec![op("locator.count", legacy.clone())]).await?,
        0,
    ) > 0
    {
        call(page, vec![op("locator.click", legacy)]).await?;
        call(page, vec![json!({"op":"waitForTimeout", "ms":400})]).await?;
    }
    for selector in ["[role='menuitemradio']", "[role='menuitem']"] {
        let items = css(selector);
        let count = count_result(
            &call(page, vec![op("locator.count", items.clone())]).await?,
            0,
        );
        for index in 0..count {
            let item = chained(items.clone(), "nth", Some(index));
            let name = string_result(
                &call(page, vec![op("locator.text", item.clone())]).await?,
                0,
            )
            .unwrap_or("")
            .to_owned();
            if name.to_lowercase().starts_with(&code_name.to_lowercase()) {
                call(page, vec![op("locator.click", item)]).await?;
                return Ok(true);
            }
        }
    }
    call(page, vec![json!({"op":"keyboard.press", "key":"Escape"})]).await?;
    Ok(false)
}

async fn wait_response<F>(
    page: &dyn PageClient,
    cursor: u64,
    timeout_ms: u64,
    mut predicate: F,
) -> Result<BrowserEvent, AdapterError>
where
    F: FnMut(&BrowserEvent) -> bool,
{
    let started = Instant::now();
    let mut after = cursor;
    while started.elapsed() < Duration::from_millis(timeout_ms.max(1)) {
        for event in page.poll_events(after).await? {
            after = after.max(event.sequence);
            if event.event_type == "response" && predicate(&event) {
                return Ok(event);
            }
        }
        call(page, vec![json!({"op":"waitForTimeout", "ms":200})]).await?;
    }
    Err(AdapterError::normalized(
        "等待 ChatGPT 回复超时",
        "TIMEOUT_ERROR",
        true,
    ))
}

/// Parse ChatGPT's newline-delimited SSE payload. Only the assistant's final
/// text channel is returned; commentary/thinking deltas are deliberately ignored.
pub fn parse_conversation_sse(body: &str) -> String {
    let mut text = String::new();
    let mut saw_final = false;
    for line in body.lines().filter_map(|line| line.strip_prefix("data: ")) {
        let data: Value = match serde_json::from_str(line.trim()) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let message = data.pointer("/v/message");
        if message
            .and_then(|v| v.pointer("/author/role"))
            .and_then(Value::as_str)
            == Some("assistant")
            && message
                .and_then(|v| v.get("channel"))
                .and_then(Value::as_str)
                == Some("final")
            && message
                .and_then(|v| v.pointer("/content/content_type"))
                .and_then(Value::as_str)
                == Some("text")
        {
            saw_final = true;
            text = message
                .and_then(|v| v.pointer("/content/parts/0"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
        }
        if saw_final {
            if data.get("o").and_then(Value::as_str) == Some("append")
                && data.get("p").and_then(Value::as_str) == Some("/message/content/parts/0")
            {
                if let Some(value) = data.get("v").and_then(Value::as_str) {
                    text.push_str(value);
                }
            }
            if let Some(patches) = data.get("v").and_then(Value::as_array) {
                for patch in patches {
                    if patch.get("o").and_then(Value::as_str) == Some("append")
                        && patch.get("p").and_then(Value::as_str)
                            == Some("/message/content/parts/0")
                    {
                        if let Some(value) = patch.get("v").and_then(Value::as_str) {
                            text.push_str(value);
                        }
                    }
                }
            }
        }
    }
    text
}

fn clean_dom_text(value: &str) -> Option<String> {
    let text = value
        .strip_prefix("ChatGPT said:")
        .unwrap_or(value)
        .replace('\u{00a0}', " ")
        .trim()
        .to_owned();
    let rejected = ["Thinking", "Instant", "Pro", "ChatGPT"];
    if text.is_empty()
        || rejected.contains(&text.as_str())
        || text.lines().all(|line| {
            let line = line.trim();
            rejected.contains(&line)
                || line.is_empty()
                || line
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == '/' || c.is_whitespace())
        })
    {
        None
    } else {
        Some(text)
    }
}

async fn extract_dom_text(page: &dyn PageClient) -> Result<String, AdapterError> {
    let nodes = css("[data-message-author-role='assistant']");
    let count = count_result(
        &call(page, vec![op("locator.count", nodes.clone())]).await?,
        0,
    );
    for index in (0..count).rev() {
        let node = chained(nodes.clone(), "nth", Some(index));
        for selector in [".markdown", ".prose", "[data-message-content-part]"] {
            let locator = descendant(node.clone(), selector);
            let n = count_result(
                &call(page, vec![op("locator.count", locator.clone())]).await?,
                0,
            );
            for child in (0..n).rev() {
                let candidate = chained(locator.clone(), "nth", Some(child));
                if let Some(text) =
                    string_result(&call(page, vec![op("locator.text", candidate)]).await?, 0)
                        .and_then(clean_dom_text)
                {
                    return Ok(text);
                }
            }
        }
        if let Some(text) = string_result(&call(page, vec![op("locator.text", node)]).await?, 0)
            .and_then(clean_dom_text)
        {
            return Ok(text);
        }
    }
    Ok(String::new())
}

fn descendant(mut locator: Value, selector: &str) -> Value {
    locator
        .as_object_mut()
        .expect("locator descriptor is an object")
        .entry("chain")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .expect("locator chain is an array")
        .push(json!({"op":"locator", "selector":selector}));
    locator
}

async fn is_generating(page: &dyn PageClient) -> Result<bool, AdapterError> {
    let body = string_result(
        &call(
            page,
            vec![json!({"op":"evaluate", "expression":"document.bodyText"})],
        )
        .await?,
        0,
    )
    .unwrap_or("")
    .to_owned();
    if ["Thinking...", "Thinking…", "正在思考", "思考中"]
        .iter()
        .any(|needle| body.contains(needle))
    {
        return Ok(true);
    }
    let stop = css(
        "button[aria-label*='stop generating' i], button[aria-label*='stop streaming' i], button[aria-label*='停止生成'], button[aria-label*='停止回答'], button[aria-label*='cancel' i]",
    );
    Ok(count_result(&call(page, vec![op("locator.count", stop)]).await?, 0) > 0)
}

async fn dom_fallback(page: &dyn PageClient, timeout_ms: u64) -> Result<String, AdapterError> {
    let started = Instant::now();
    let timeout = Duration::from_millis(timeout_ms.min(60_000));
    let mut previous = String::new();
    let mut stable = 0_u8;
    while started.elapsed() < timeout {
        let current = extract_dom_text(page).await.unwrap_or_default();
        let generating = is_generating(page).await.unwrap_or(false);
        if !current.is_empty() && current == previous && !generating {
            stable = stable.saturating_add(1);
        } else {
            stable = 0;
            if !current.is_empty() {
                previous = current;
            }
        }
        if !previous.is_empty() && !generating && stable >= 5 {
            return Ok(previous.trim().to_owned());
        }
        call(page, vec![json!({"op":"waitForTimeout", "ms":1200})]).await?;
    }
    Ok(previous.trim().to_owned())
}

async fn upload_inputs(
    page: &dyn PageClient,
    paths: &[std::path::PathBuf],
    timeout_ms: u64,
) -> Result<(), AdapterError> {
    let cursor = page.subscribe_responses().await?;
    call(page, vec![json!({
        "op":"filechooser.clickAndSetFiles", "locator":role("button", "Add files and more"),
        "files":paths.iter().map(|path| path.to_string_lossy().to_string()).collect::<Vec<_>>(),
        "clickOptions":{"clickCount":2, "timeout":30000}, "fileOptions":{"timeout":30000}, "options":{"timeout":30000}
    })]).await?;
    let started = Instant::now();
    let mut after = cursor;
    let mut processed = 0;
    while started.elapsed() < Duration::from_millis(timeout_ms.max(1)) {
        for event in page.poll_events(after).await? {
            after = after.max(event.sequence);
            if event.event_type == "response"
                && event.status == Some(200)
                && event
                    .url
                    .as_deref()
                    .is_some_and(|url| url.contains("backend-api/files/process_upload_stream"))
            {
                processed += 1;
                if processed >= paths.len() {
                    return Ok(());
                }
            }
        }
        call(page, vec![json!({"op":"waitForTimeout", "ms":200})]).await?;
    }
    Err(AdapterError::normalized(
        format!("图片上传处理超时 ({processed}/{})", paths.len()),
        "TIMEOUT_ERROR",
        true,
    ))
}

async fn generate(
    page: &dyn PageClient,
    request: &GenerateRequest,
) -> Result<AdapterOutput, AdapterError> {
    let target_url = target_url_for(&request.adapter_config);
    let nav = call(page, vec![json!({"op":"goto", "url":target_url, "options":{"waitUntil":"load", "timeout":120000}})])
        .await.map_err(|error| page_error(&error.message).unwrap_or(error))?;
    if let Some(status) = nav
        .first()
        .and_then(|value| value.get("status"))
        .and_then(Value::as_u64)
        .filter(|status| *status >= 400)
    {
        return Err(AdapterError::normalized(
            format!("网站无法访问 (HTTP {status})"),
            "HTTP_ERROR",
            status >= 500,
        ));
    }
    let input = wait_for_input(page, 90_000).await?;
    if let Some((_, code_name)) = CHATGPT_TEXT_MODELS
        .iter()
        .find(|(id, _)| *id == request.model_id)
    {
        let _ = select_model(page, code_name).await;
    }
    if !request.image_paths.is_empty() {
        upload_inputs(
            page,
            &request.image_paths,
            request.wait_timeout_ms.max(120_000),
        )
        .await?;
    }
    call(page, vec![
        op("locator.click", input.clone()),
        json!({"op":"locator.fill", "locator":input, "value":request.prompt, "options":{"timeout":10000}}),
    ]).await?;
    let cursor = page.subscribe_responses().await?;
    call(page, vec![json!({"op":"keyboard.press", "key":"Enter"})]).await?;
    let event = wait_response(page, cursor, request.wait_timeout_ms, |event| {
        event
            .request_method
            .as_deref()
            .is_some_and(|method| method.eq_ignore_ascii_case("POST"))
            && event
                .url
                .as_deref()
                .is_some_and(|url| url.contains("backend-api/f/conversation"))
    })
    .await
    .map_err(|error| page_error(&error.message).unwrap_or(error))?;
    let response_id = event
        .response_id
        .as_deref()
        .ok_or_else(|| AdapterError::new("ChatGPT 响应缺少 responseId"))?;
    page.response_wait_finished(response_id, request.wait_timeout_ms)
        .await?;
    let body = String::from_utf8_lossy(&page.response_body(response_id).await?).into_owned();
    if let Some(error) = http_error(event.status.unwrap_or(200), &body) {
        return Err(error);
    }
    let mut text = parse_conversation_sse(&body).trim().to_owned();
    if text.is_empty() {
        text = dom_fallback(page, request.wait_timeout_ms).await?;
    }
    if text.trim().is_empty() {
        return Ok(AdapterOutput::error("回复内容为空"));
    }
    Ok(AdapterOutput::text(text.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::client::mock::MockPage;

    #[test]
    fn metadata_model_names_and_target_match_legacy_manifest() {
        let adapter = ChatGptTextAdapter;
        assert_eq!(adapter.id(), "chatgpt_text");
        assert_eq!(adapter.target_url(), "https://chatgpt.com/");
        assert_eq!(
            adapter.model_ids(),
            &["gpt-instant", "gpt-thinking", "gpt-pro"]
        );
        assert_eq!(
            CHATGPT_TEXT_MODELS,
            &[
                ("gpt-instant", "Instant"),
                ("gpt-thinking", "Thinking"),
                ("gpt-pro", "Pro")
            ]
        );
    }

    #[test]
    fn temporary_chat_setting_is_honored_in_both_config_shapes() {
        assert_eq!(
            target_url_for(&json!({"temporaryChat":true})),
            "https://chatgpt.com/?temporary-chat=true"
        );
        assert_eq!(
            target_url_for(&json!({"backend":{"adapter":{"chatgpt_text":{"temporaryChat":true}}}})),
            "https://chatgpt.com/?temporary-chat=true"
        );
        assert_eq!(target_url_for(&Value::Null), CHATGPT_TEXT_TARGET_URL);
    }

    #[test]
    fn sse_parser_only_collects_final_assistant_text() {
        let body = concat!(
            "data: {\"v\":{\"message\":{\"author\":{\"role\":\"assistant\"},\"channel\":\"analysis\",\"content\":{\"content_type\":\"text\",\"parts\":[\"hidden\"]}}}}\n",
            "data: {\"v\":{\"message\":{\"id\":\"m1\",\"author\":{\"role\":\"assistant\"},\"channel\":\"final\",\"content\":{\"content_type\":\"text\",\"parts\":[\"Hello\"]}}}}\n",
            "data: {\"o\":\"append\",\"p\":\"/message/content/parts/0\",\"v\":\" there\"}\n",
            "data: {\"v\":[{\"o\":\"append\",\"p\":\"/message/content/parts/0\",\"v\":\"!\"}]}\n"
        );
        assert_eq!(parse_conversation_sse(body), "Hello there!");
    }

    #[test]
    fn dom_fallback_filters_model_labels_and_cleans_prefix() {
        assert_eq!(
            clean_dom_text("ChatGPT said: Hello\u{00a0}there").as_deref(),
            Some("Hello there")
        );
        assert_eq!(clean_dom_text("Thinking"), None);
        assert_eq!(clean_dom_text("2 / 2"), None);
    }

    #[tokio::test]
    async fn unknown_model_fails_before_any_browser_operation() {
        let page = MockPage::default();
        let request = GenerateRequest {
            prompt: "x".into(),
            model_id: "gpt-unknown".into(),
            image_paths: vec![],
            reasoning: false,
            wait_timeout_ms: 1000,
            adapter_config: Value::Null,
        };
        let output = ChatGptTextAdapter.generate(&page, &request).await.unwrap();
        assert_eq!(output.error.as_deref(), Some("未找到模型配置: gpt-unknown"));
        assert!(page.calls.lock().unwrap().is_empty());
    }
}
