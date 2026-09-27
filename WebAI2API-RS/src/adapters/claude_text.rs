use super::client::{
    chained, count_result, css, http_error, op, page_error, role, string_result, AdapterError,
    AdapterOutput, BrowserEvent, ClientFuture, GenerateRequest, PageClient, SiteAdapter,
};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

pub const CLAUDE_TARGET_URL: &str = "https://claude.ai/new";
const INPUT_SELECTOR: &str = "[contenteditable=\"true\"], textarea";
pub const CLAUDE_MODELS: &[(&str, &str)] = &[("claude-sonnet-5", "Sonnet"), ("claude-auto", "")];

#[derive(Debug, Default, Clone, Copy)]
pub struct ClaudeTextAdapter;

impl SiteAdapter for ClaudeTextAdapter {
    fn id(&self) -> &'static str {
        "claude_text"
    }
    fn target_url(&self) -> &'static str {
        CLAUDE_TARGET_URL
    }
    fn model_ids(&self) -> &'static [&'static str] {
        &["claude-sonnet-5", "claude-auto"]
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
            if !request.image_paths.is_empty() {
                return Ok(AdapterOutput::error("Claude 网页适配器暂不支持图片输入"));
            }
            match generate(
                page,
                &request.prompt,
                &request.model_id,
                request.wait_timeout_ms.max(1),
            )
            .await
            {
                Ok(output) => Ok(output),
                Err(error) => Ok(AdapterOutput::normalized_error(error)),
            }
        })
    }
}

async fn call(page: &dyn PageClient, operations: Vec<Value>) -> Result<Vec<Value>, AdapterError> {
    page.call(operations).await
}

async fn first_locator_count(page: &dyn PageClient, locator: Value) -> Result<usize, AdapterError> {
    let values = call(page, vec![op("locator.count", locator)]).await?;
    Ok(count_result(&values, 0))
}

async fn wait_for_input(page: &dyn PageClient, timeout_ms: u64) -> Result<Value, AdapterError> {
    let locator = chained(css(INPUT_SELECTOR), "last", None);
    let started = Instant::now();
    loop {
        if first_locator_count(page, locator.clone()).await? > 0 {
            let values = call(page, vec![op("locator.visible", locator.clone())]).await?;
            if values.first().and_then(Value::as_bool).unwrap_or(false) {
                return Ok(locator);
            }
        }
        if started.elapsed() >= Duration::from_millis(timeout_ms) {
            return Err(AdapterError::new(
                "未找到输入框 ([contenteditable=\"true\"], textarea)",
            ));
        }
        call(page, vec![json!({"op":"waitForTimeout", "ms":500})]).await?;
    }
}

async fn select_model(page: &dyn PageClient, code_name: &str) -> Result<bool, AdapterError> {
    if code_name.is_empty() {
        return Ok(false);
    }

    let mut trigger = None;
    let candidates = [
        css("[data-testid*='model' i]"),
        role("button", "model"),
        css("button"),
    ];
    for candidate in candidates {
        let count = first_locator_count(page, candidate.clone()).await?;
        for index in 0..count {
            let locator = chained(candidate.clone(), "nth", Some(index));
            let values = call(page, vec![op("locator.text", locator.clone())]).await?;
            let text = string_result(&values, 0).unwrap_or("").to_ascii_lowercase();
            if ["claude", "sonnet", "haiku", "opus", "model"]
                .iter()
                .any(|word| text.contains(word))
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
    call(page, vec![json!({"op":"waitForTimeout", "ms":650})]).await?;
    for selector in ["[role='menuitem']", "[role='option']", "button"] {
        let items = css(selector);
        let count = first_locator_count(page, items.clone()).await?;
        for index in 0..count {
            let item = chained(items.clone(), "nth", Some(index));
            let values = call(page, vec![op("locator.text", item.clone())]).await?;
            if string_result(&values, 0)
                .unwrap_or("")
                .to_lowercase()
                .contains(&code_name.to_lowercase())
            {
                call(page, vec![op("locator.click", item)]).await?;
                call(page, vec![json!({"op":"waitForTimeout", "ms":400})]).await?;
                return Ok(true);
            }
        }
    }
    call(page, vec![json!({"op":"keyboard.press", "key":"Escape"})]).await?;
    Ok(false)
}

async fn click_send(page: &dyn PageClient, input: &Value) -> Result<(), AdapterError> {
    for selector in [
        "[data-testid*='send' i]",
        "button[aria-label*='Send' i], button[aria-label*='发送']",
    ] {
        let buttons = css(selector);
        let count = first_locator_count(page, buttons.clone()).await?;
        for index in (0..count).rev() {
            let button = chained(buttons.clone(), "nth", Some(index));
            let visible = call(page, vec![op("locator.visible", button.clone())])
                .await?
                .first()
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !visible {
                continue;
            }
            let enabled = call(page, vec![op("locator.enabled", button.clone())])
                .await?
                .first()
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !enabled {
                continue;
            }
            match call(page, vec![op("locator.click", button)]).await {
                Ok(_) => return Ok(()),
                Err(_) => continue,
            }
        }
    }
    call(
        page,
        vec![
            op("locator.click", input.clone()),
            json!({"op":"keyboard.press", "key":"Enter"}),
        ],
    )
    .await?;
    Ok(())
}

fn acceptable_text(value: &str, prompt: &str) -> Option<String> {
    let cleaned = value
        .replace("Claude said:", "")
        .replace('\u{00a0}', " ")
        .trim()
        .to_owned();
    if cleaned.is_empty()
        || ["Claude", "Thinking", "思考中"].contains(&cleaned.as_str())
        || cleaned == prompt.trim()
    {
        None
    } else {
        Some(cleaned)
    }
}

async fn extract_assistant_text(
    page: &dyn PageClient,
    prompt: &str,
) -> Result<String, AdapterError> {
    let selectors = [
        "[data-testid*='assistant' i]",
        "[data-message-author-role='assistant']",
        "[class*='assistant' i]",
        "[class*='message' i]",
    ];
    for selector in selectors {
        let nodes = css(selector);
        let count = first_locator_count(page, nodes.clone()).await?;
        for index in (0..count).rev() {
            let node = chained(nodes.clone(), "nth", Some(index));
            let values = call(page, vec![op("locator.text", node)]).await?;
            if let Some(text) =
                string_result(&values, 0).and_then(|text| acceptable_text(text, prompt))
            {
                return Ok(text);
            }
        }
    }
    Ok(String::new())
}

async fn is_generating(page: &dyn PageClient) -> Result<bool, AdapterError> {
    let values = call(
        page,
        vec![json!({"op":"evaluate", "expression":"document.bodyText"})],
    )
    .await?;
    let body = string_result(&values, 0).unwrap_or("");
    if [
        "Thinking...",
        "Thinking…",
        "思考中",
        "Claude is responding",
        "正在回复",
    ]
    .iter()
    .any(|text| body.contains(text))
    {
        return Ok(true);
    }
    for selector in [
        "button[aria-label*='stop' i], button[aria-label*='停止']",
        "button[aria-label*='cancel' i], button[aria-label*='取消']",
    ] {
        if first_locator_count(page, css(selector)).await? > 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

fn normalize_final_text(value: &str) -> String {
    let text = value
        .trim()
        .strip_prefix("Claude responded:")
        .unwrap_or(value.trim())
        .trim();
    let mut lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.to_ascii_lowercase().starts_with("thought for "))
        .collect();
    if lines.len() >= 2 && lines[0] == lines[1] {
        lines.remove(0);
    }
    lines.join("\n").trim().to_owned()
}

async fn wait_for_final_text(
    page: &dyn PageClient,
    prompt: &str,
    timeout_ms: u64,
) -> Result<String, AdapterError> {
    let started = Instant::now();
    let timeout = Duration::from_millis(timeout_ms.min(180_000));
    let mut previous = String::new();
    let mut stable_count = 0_u8;
    while started.elapsed() < timeout {
        let current = extract_assistant_text(page, prompt)
            .await
            .unwrap_or_default();
        let generating = is_generating(page).await.unwrap_or(false);
        if !current.is_empty() && current == previous && !generating {
            stable_count = stable_count.saturating_add(1);
        } else {
            stable_count = 0;
            if !current.is_empty() {
                previous = current;
            }
        }
        if !previous.is_empty() && !generating && stable_count >= 5 {
            break;
        }
        call(page, vec![json!({"op":"waitForTimeout", "ms":1400})]).await?;
    }
    Ok(normalize_final_text(&previous))
}

fn parse_response_error(status: u16, body: &str) -> Option<AdapterError> {
    http_error(status, body)
}

async fn wait_response(
    page: &dyn PageClient,
    cursor: u64,
    timeout_ms: u64,
) -> Result<BrowserEvent, AdapterError> {
    let started = Instant::now();
    let mut after = cursor;
    while started.elapsed() < Duration::from_millis(timeout_ms) {
        let events = page.poll_events(after).await?;
        for event in events {
            after = after.max(event.sequence);
            if event.event_type == "response"
                && event
                    .request_method
                    .as_deref()
                    .is_some_and(|method| method.eq_ignore_ascii_case("POST"))
                && event.url.as_deref().is_some_and(|url| {
                    url.contains("api/organizations") && url.contains("/completion")
                })
            {
                return Ok(event);
            }
        }
        call(page, vec![json!({"op":"waitForTimeout", "ms":250})]).await?;
    }
    Err(AdapterError::normalized(
        "等待 Claude 回复超时",
        "TIMEOUT_ERROR",
        true,
    ))
}

async fn generate(
    page: &dyn PageClient,
    prompt: &str,
    model_id: &str,
    timeout_ms: u64,
) -> Result<AdapterOutput, AdapterError> {
    let nav = call(page, vec![json!({"op":"goto", "url":CLAUDE_TARGET_URL, "options":{"waitUntil":"load", "timeout":120000}})])
        .await
        .map_err(|error| page_error(&error.message).unwrap_or(error))?;
    let status = nav
        .first()
        .and_then(|value| value.get("status"))
        .and_then(Value::as_u64);
    let Some(status) = status else {
        return Err(AdapterError::normalized(
            "页面加载失败: 无响应",
            "TIMEOUT_ERROR",
            true,
        ));
    };
    if status >= 400 {
        return Err(AdapterError::normalized(
            format!("网站无法访问 (HTTP {status})"),
            "HTTP_ERROR",
            status >= 500,
        ));
    }
    let input = wait_for_input(page, 90_000).await?;
    let code_name = CLAUDE_MODELS
        .iter()
        .find(|(id, _)| *id == model_id)
        .map(|(_, code)| *code)
        .unwrap_or("");
    if !code_name.is_empty() {
        let _ = select_model(page, code_name).await;
    }
    call(page, vec![op("locator.click", input.clone())]).await?;
    // `locator.fill` is a generic Playwright operation; keep the prompt as its
    // explicit value rather than using adapter-specific JavaScript typing.
    call(page, vec![json!({"op":"locator.fill", "locator":input, "value":prompt, "options":{"timeout":10000}})]).await?;
    call(page, vec![json!({"op":"waitForTimeout", "ms":400})]).await?;
    let cursor = page.subscribe_responses().await?;
    click_send(page, &chained(css(INPUT_SELECTOR), "last", None)).await?;
    let event = wait_response(page, cursor, timeout_ms).await?;
    let response_id = event
        .response_id
        .ok_or_else(|| AdapterError::new("Claude 响应缺少 responseId"))?;
    let body = page.response_body(&response_id).await?;
    let body_text = String::from_utf8_lossy(&body);
    if let Some(error) = parse_response_error(event.status.unwrap_or(200), &body_text) {
        return Err(error);
    }
    let text = wait_for_final_text(page, prompt, timeout_ms).await?;
    if text.is_empty() {
        return Ok(AdapterOutput::error(
            "Claude 回复内容为空；请确认已登录且账号仍有可用额度",
        ));
    }
    Ok(AdapterOutput::text(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::client::mock::MockPage;

    #[test]
    fn metadata_matches_legacy_manifest() {
        let adapter = ClaudeTextAdapter;
        assert_eq!(adapter.id(), "claude_text");
        assert_eq!(adapter.target_url(), "https://claude.ai/new");
        assert_eq!(adapter.model_ids(), &["claude-sonnet-5", "claude-auto"]);
        assert_eq!(
            CLAUDE_MODELS,
            &[("claude-sonnet-5", "Sonnet"), ("claude-auto", "")]
        );
    }

    #[test]
    fn final_text_cleanup_matches_legacy_contract() {
        assert_eq!(
            normalize_final_text("Claude responded: Thought for 2s\nHello\nHello"),
            "Hello"
        );
        assert_eq!(acceptable_text("Claude said: Thinking", "prompt"), None);
        assert_eq!(acceptable_text("answer", "prompt"), Some("answer".into()));
    }

    #[tokio::test]
    async fn unsupported_model_fails_before_browser_calls() {
        let page = MockPage::default();
        let request = GenerateRequest {
            prompt: "x".into(),
            model_id: "missing".into(),
            image_paths: vec![],
            reasoning: false,
            wait_timeout_ms: 100,
            adapter_config: Value::Null,
        };
        let output = ClaudeTextAdapter.generate(&page, &request).await.unwrap();
        assert_eq!(output.error.as_deref(), Some("未找到模型配置: missing"));
        assert!(page.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn response_http_error_contract_is_preserved() {
        let error = parse_response_error(429, "Too Many Requests").unwrap();
        assert_eq!(error.code.as_deref(), Some("RATE_LIMITED"));
        assert_eq!(error.retryable, Some(true));
        let error = parse_response_error(422, "{\"error\":\"prompt rejected\"}").unwrap();
        assert_eq!(error.code.as_deref(), Some("CONTENT_BLOCKED"));
        assert_eq!(error.retryable, Some(false));
    }

    #[tokio::test]
    async fn generation_uses_generic_navigation_input_send_and_response_contract() {
        use crate::adapters::client::BrowserEvent;

        let results = [
            json!({"status":200, "url":CLAUDE_TARGET_URL}),
            json!(1),
            json!(true), // input ready
            json!(1),
            json!("Claude Sonnet"),
            json!(null),
            json!(null), // model selector
            json!(1),
            json!("Sonnet"),
            json!(null),
            json!(null), // choose model
            json!(null),
            json!(null),
            json!(null), // click, fill, settle
            json!(1),
            json!(true),
            json!(true),
            json!(null), // send button
            json!(1),
            json!("Hello from Claude"),
            json!(""),
            json!(0),
            json!(0),
            json!(null),
            json!(1),
            json!("Hello from Claude"),
            json!(""),
            json!(0),
            json!(0),
            json!(null),
            json!(1),
            json!("Hello from Claude"),
            json!(""),
            json!(0),
            json!(0),
            json!(null),
            json!(1),
            json!("Hello from Claude"),
            json!(""),
            json!(0),
            json!(0),
            json!(null),
            json!(1),
            json!("Hello from Claude"),
            json!(""),
            json!(0),
            json!(0),
            json!(null),
            json!(1),
            json!("Hello from Claude"),
            json!(""),
            json!(0),
            json!(0),
            json!(null),
        ];
        let page = MockPage::returning(results);
        *page.events.lock().unwrap() = vec![BrowserEvent {
            sequence: 1,
            event_type: "response".into(),
            browser_id: Some("b".into()),
            page_id: Some("p".into()),
            generation: Some(1),
            response_id: Some("response-1".into()),
            url: Some(
                "https://claude.ai/api/organizations/org/chat_conversations/chat/completion".into(),
            ),
            status: Some(200),
            request_method: Some("POST".into()),
            headers: None,
            route_token: None,
            request: None,
        }];
        *page.body.lock().unwrap() = "{}".into();
        let request = GenerateRequest {
            prompt: "Hello".into(),
            model_id: "claude-sonnet-5".into(),
            image_paths: vec![],
            reasoning: false,
            wait_timeout_ms: 100,
            adapter_config: Value::Null,
        };

        let output = ClaudeTextAdapter.generate(&page, &request).await.unwrap();
        assert_eq!(output.text.as_deref(), Some("Hello from Claude"));
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
            .any(|op| op["op"] == "goto" && op["url"] == CLAUDE_TARGET_URL));
        assert!(operations
            .iter()
            .any(|op| op["op"] == "locator.fill" && op["value"] == "Hello"));
        assert!(
            operations
                .iter()
                .any(|op| op["op"] == "keyboard.press" && op["key"] == "Enter")
                || operations.iter().any(|op| op["op"] == "locator.click")
        );
    }
}
