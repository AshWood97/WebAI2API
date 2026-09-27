use super::super::client::{
    http_error, page_error, AdapterError, AdapterOutput, ClientFuture, GenerateRequest, PageClient,
    SiteAdapter,
};
use serde_json::{json, Value};
use std::time::Duration;

const TARGET_URL: &str = "https://arena.ai/text/direct";
const TARGET_URL_SEARCH: &str = "https://arena.ai/search/direct";

// Model IDs, picker aliases, and search routing are generated from the legacy manifest.
const MODEL_META: &[(&str, Option<&str>, bool)] = &[
    ("claude-sonnet-4-5-20250929", None, false),
    ("gemini-2.5-pro", None, false),
    ("claude-haiku-4-5-20251001", None, false),
    ("gemini-3-flash", None, false),
    ("gpt-5.2-high", None, false),
    ("gpt-5.1", None, false),
    ("gpt-5.2", None, false),
    ("grok-4.20-beta-0309-reasoning", None, false),
    ("gpt-5.2-chat-latest", None, false),
    ("grok-4.20-multi-agent-beta-0309", None, false),
    ("grok-4.1-thinking", None, false),
    ("qwen3.5-max-preview", None, false),
    ("claude-sonnet-4-6", None, false),
    ("grok-4.1", None, false),
    ("gpt-5.4-mini-high", None, false),
    ("gpt-5.3-chat-latest", None, false),
    ("glm-5", None, false),
    ("gpt-5.1-high", None, false),
    ("claude-sonnet-4-5-20250929-thinking-32k", None, false),
    ("ernie-5.0-0110", None, false),
    ("mimo-v2-pro", None, false),
    ("longcat-flash-chat-2602-exp", None, false),
    ("glm-4.7", None, false),
    ("gemini-3.1-flash-lite-preview", None, false),
    ("qwen3-max-preview", None, false),
    ("gpt-5-high", None, false),
    ("kimi-k2.5-instant", None, false),
    ("o3-2025-04-16", None, false),
    ("grok-4-1-fast-reasoning", None, false),
    ("kimi-k2-thinking-turbo", None, false),
    ("gpt-5-chat", None, false),
    ("qwen3-max-2025-09-23", None, false),
    ("deepseek-v3.2", None, false),
    ("qwen3-235b-a22b-instruct-2507", None, false),
    ("deepseek-v3.2-thinking", None, false),
    ("grok-4-fast-chat", None, false),
    ("kimi-k2-0905-preview", None, false),
    ("qwen3.5-122b-a10b", None, false),
    ("kimi-k2-0711-preview", None, false),
    ("qwen3-vl-235b-a22b-instruct", None, false),
    ("mistral-large-3", None, false),
    ("gpt-4.1-2025-04-14", None, false),
    ("gemini-2.5-flash", None, false),
    ("mistral-medium-2508", None, false),
    ("grok-4-0709", None, false),
    ("grok-4-fast-reasoning", None, false),
    ("minimax-m2.5", None, false),
    ("qwen3.5-27b", None, false),
    ("minimax-m2.7", None, false),
    ("gpt-5.4-nano-high", None, false),
    ("qwen3-235b-a22b-no-thinking", None, false),
    ("qwen3-next-80b-a3b-instruct", None, false),
    ("longcat-flash-chat", None, false),
    ("qwen3.5-flash", None, false),
    ("qwen3-235b-a22b-thinking-2507", None, false),
    ("claude-sonnet-4-20250514-thinking-32k", None, false),
    ("hunyuan-vision-1.5-thinking", None, false),
    ("qwen3.5-35b-a3b", None, false),
    ("qwen3-vl-235b-a22b-thinking", None, false),
    ("mimo-v2-flash", None, false),
    (
        "mimo-v2-flash-thinking",
        Some("mimo-v2-flash (thinking)"),
        false,
    ),
    ("step-3.5-flash", None, false),
    ("o4-mini-2025-04-16", None, false),
    ("gpt-5-mini-high", None, false),
    ("claude-sonnet-4-20250514", None, false),
    ("qwen3-coder-480b-a35b-instruct", None, false),
    ("hunyuan-t1-20250711", None, false),
    ("claude-3-7-sonnet-20250219-thinking-32k", None, false),
    ("mistral-medium-2505", None, false),
    ("minimax-m2.1-preview", None, false),
    ("qwen3-30b-a3b-instruct-2507", None, false),
    ("gpt-4.1-mini-2025-04-14", None, false),
    ("trinity-large", None, false),
    ("qwen3-235b-a22b", None, false),
    ("claude-3-5-sonnet-20241022", None, false),
    ("claude-3-7-sonnet-20250219", None, false),
    ("qwen3-next-80b-a3b-thinking", None, false),
    ("gemma-3-27b-it", None, false),
    ("minimax-m1", None, false),
    ("grok-3-mini-high", None, false),
    ("gemini-2.0-flash-001", None, false),
    ("grok-3-mini-beta", None, false),
    ("mistral-small-2506", None, false),
    ("intellect-3", None, false),
    ("gpt-oss-120b", None, false),
    ("step-3", None, false),
    ("o3-mini", None, false),
    ("mercury-2", None, false),
    ("minimax-m2", None, false),
    ("ling-flash-2.0", None, false),
    ("nova-2-lite", None, false),
    ("gpt-5-nano-high", None, false),
    ("qwq-32b", None, false),
    ("olmo-3.1-32b-instruct", None, false),
    ("qwen3-30b-a3b", None, false),
    ("molmo-2-8b", None, false),
    ("ring-flash-2.0", None, false),
    ("gemma-3n-e4b-it", None, false),
    ("llama-3.3-70b-instruct", None, false),
    ("nvidia-nemotron-3-nano-30b-a3b-bf16", None, false),
    ("gpt-oss-20b", None, false),
    ("mercury", None, false),
    ("olmo-3-32b-think", None, false),
    ("mistral-small-3.1-24b-instruct-2503", None, false),
    ("ibm-granite-h-small", None, false),
    ("olmo-3.1-32b-think", None, false),
    ("ling-2.5-1t", None, false),
    ("ring-2.5-1t", None, false),
    ("seed-1.8", None, false),
    ("dola-seed-2.0-preview-vision", None, false),
    ("grok-4-1-fast-non-reasoning", None, false),
    ("amazon.nova-pro-v1:0", None, false),
    ("qwen3.6-plus", None, false),
    ("qwen3.5-397b-a17b", None, false),
    ("glm-5.1", None, false),
    ("trinity-large-thinking", None, false),
    ("mimo-v2-omni", None, false),
    ("kimi-k2.5", None, false),
    ("gpt-5-high-new-system-prompt", None, false),
    ("qwen3-vl-8b-thinking", None, false),
    ("ernie-5.0-preview-1220", None, false),
    ("qwen3-vl-8b-instruct", None, false),
    ("glm-5v-turbo", None, false),
    ("glm-4.7-flash", None, false),
    (
        "gemini-3-flash-thinking-minimal",
        Some("gemini-3-flash (thinking-minimal)"),
        false,
    ),
    ("mistral-small-2603", None, false),
    ("grok-4.20-beta1", None, false),
    ("dola-seed-2.0-preview-text", None, false),
    ("qwen3-max-2025-09-26", None, false),
    ("qwen3-omni-flash", None, false),
    ("qwen-vl-max-2025-08-13", None, false),
    ("minimax-m2-preview", None, false),
    ("qwen3-max-thinking", None, false),
    ("gemma-4-26b-a4b", None, false),
    ("gemma-4-31b", None, false),
    ("claude-sonnet-4-6-search", None, true),
    ("gpt-5.2-search", None, true),
    ("gemini-3-flash-grounding", None, true),
    ("grok-4.20-multi-agent-beta-0309", None, true),
    ("grok-4.20-beta1", None, true),
    ("gpt-5.1-search", None, true),
    ("grok-4-1-fast-search", None, true),
    ("gpt-5.2-search-non-reasoning", None, true),
    ("grok-4-fast-search", None, true),
    ("o3-search", None, true),
    ("gemini-2.5-pro-grounding", None, true),
    ("claude-sonnet-4-5-search", None, true),
    ("grok-4-search", None, true),
    ("ppl-sonar-reasoning-pro-high", None, true),
    ("gpt-5-search", None, true),
    ("gpt-5.1-search-sp", None, true),
];
const MODEL_IDS: &[&str] = &[
    "claude-sonnet-4-5-20250929",
    "gemini-2.5-pro",
    "claude-haiku-4-5-20251001",
    "gemini-3-flash",
    "gpt-5.2-high",
    "gpt-5.1",
    "gpt-5.2",
    "grok-4.20-beta-0309-reasoning",
    "gpt-5.2-chat-latest",
    "grok-4.20-multi-agent-beta-0309",
    "grok-4.1-thinking",
    "qwen3.5-max-preview",
    "claude-sonnet-4-6",
    "grok-4.1",
    "gpt-5.4-mini-high",
    "gpt-5.3-chat-latest",
    "glm-5",
    "gpt-5.1-high",
    "claude-sonnet-4-5-20250929-thinking-32k",
    "ernie-5.0-0110",
    "mimo-v2-pro",
    "longcat-flash-chat-2602-exp",
    "glm-4.7",
    "gemini-3.1-flash-lite-preview",
    "qwen3-max-preview",
    "gpt-5-high",
    "kimi-k2.5-instant",
    "o3-2025-04-16",
    "grok-4-1-fast-reasoning",
    "kimi-k2-thinking-turbo",
    "gpt-5-chat",
    "qwen3-max-2025-09-23",
    "deepseek-v3.2",
    "qwen3-235b-a22b-instruct-2507",
    "deepseek-v3.2-thinking",
    "grok-4-fast-chat",
    "kimi-k2-0905-preview",
    "qwen3.5-122b-a10b",
    "kimi-k2-0711-preview",
    "qwen3-vl-235b-a22b-instruct",
    "mistral-large-3",
    "gpt-4.1-2025-04-14",
    "gemini-2.5-flash",
    "mistral-medium-2508",
    "grok-4-0709",
    "grok-4-fast-reasoning",
    "minimax-m2.5",
    "qwen3.5-27b",
    "minimax-m2.7",
    "gpt-5.4-nano-high",
    "qwen3-235b-a22b-no-thinking",
    "qwen3-next-80b-a3b-instruct",
    "longcat-flash-chat",
    "qwen3.5-flash",
    "qwen3-235b-a22b-thinking-2507",
    "claude-sonnet-4-20250514-thinking-32k",
    "hunyuan-vision-1.5-thinking",
    "qwen3.5-35b-a3b",
    "qwen3-vl-235b-a22b-thinking",
    "mimo-v2-flash",
    "mimo-v2-flash-thinking",
    "step-3.5-flash",
    "o4-mini-2025-04-16",
    "gpt-5-mini-high",
    "claude-sonnet-4-20250514",
    "qwen3-coder-480b-a35b-instruct",
    "hunyuan-t1-20250711",
    "claude-3-7-sonnet-20250219-thinking-32k",
    "mistral-medium-2505",
    "minimax-m2.1-preview",
    "qwen3-30b-a3b-instruct-2507",
    "gpt-4.1-mini-2025-04-14",
    "trinity-large",
    "qwen3-235b-a22b",
    "claude-3-5-sonnet-20241022",
    "claude-3-7-sonnet-20250219",
    "qwen3-next-80b-a3b-thinking",
    "gemma-3-27b-it",
    "minimax-m1",
    "grok-3-mini-high",
    "gemini-2.0-flash-001",
    "grok-3-mini-beta",
    "mistral-small-2506",
    "intellect-3",
    "gpt-oss-120b",
    "step-3",
    "o3-mini",
    "mercury-2",
    "minimax-m2",
    "ling-flash-2.0",
    "nova-2-lite",
    "gpt-5-nano-high",
    "qwq-32b",
    "olmo-3.1-32b-instruct",
    "qwen3-30b-a3b",
    "molmo-2-8b",
    "ring-flash-2.0",
    "gemma-3n-e4b-it",
    "llama-3.3-70b-instruct",
    "nvidia-nemotron-3-nano-30b-a3b-bf16",
    "gpt-oss-20b",
    "mercury",
    "olmo-3-32b-think",
    "mistral-small-3.1-24b-instruct-2503",
    "ibm-granite-h-small",
    "olmo-3.1-32b-think",
    "ling-2.5-1t",
    "ring-2.5-1t",
    "seed-1.8",
    "dola-seed-2.0-preview-vision",
    "grok-4-1-fast-non-reasoning",
    "amazon.nova-pro-v1:0",
    "qwen3.6-plus",
    "qwen3.5-397b-a17b",
    "glm-5.1",
    "trinity-large-thinking",
    "mimo-v2-omni",
    "kimi-k2.5",
    "gpt-5-high-new-system-prompt",
    "qwen3-vl-8b-thinking",
    "ernie-5.0-preview-1220",
    "qwen3-vl-8b-instruct",
    "glm-5v-turbo",
    "glm-4.7-flash",
    "gemini-3-flash-thinking-minimal",
    "mistral-small-2603",
    "grok-4.20-beta1",
    "dola-seed-2.0-preview-text",
    "qwen3-max-2025-09-26",
    "qwen3-omni-flash",
    "qwen-vl-max-2025-08-13",
    "minimax-m2-preview",
    "qwen3-max-thinking",
    "gemma-4-26b-a4b",
    "gemma-4-31b",
    "claude-sonnet-4-6-search",
    "gpt-5.2-search",
    "gemini-3-flash-grounding",
    "grok-4.20-multi-agent-beta-0309",
    "grok-4.20-beta1",
    "gpt-5.1-search",
    "grok-4-1-fast-search",
    "gpt-5.2-search-non-reasoning",
    "grok-4-fast-search",
    "o3-search",
    "gemini-2.5-pro-grounding",
    "claude-sonnet-4-5-search",
    "grok-4-search",
    "ppl-sonar-reasoning-pro-high",
    "gpt-5-search",
    "gpt-5.1-search-sp",
];

pub struct LmArenaText;

impl SiteAdapter for LmArenaText {
    fn id(&self) -> &'static str {
        "lmarena_text"
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
    let model = MODEL_META.iter().find(|(id, _, _)| *id == request.model_id);
    let target = if model.is_some_and(|(_, _, search)| *search) {
        TARGET_URL_SEARCH
    } else {
        TARGET_URL
    };
    let goto = page
        .call(vec![
            json!({"op":"goto", "url":target, "options":{"waitUntil":"load", "timeout":30000}}),
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
    page.call(vec![json!({"op":"locator.waitFor", "locator":{"kind":"css","value":TEXTAREA}, "options":{"state":"visible","timeout":120000}})]).await?;

    if !request.model_id.is_empty() {
        let search = model
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
        // Keep the original tolerance for an option list that updates slowly.
        let _ = page.call(vec![json!({"op":"locator.waitFor","locator":{"kind":"css","value":"[role=option]","chain":[{"op":"first"}]},"options":{"state":"visible","timeout":5000}}),
                               json!({"op":"locator.text","locator":{"kind":"css","value":"[role=option]","chain":[{"op":"first"}]},"options":{"timeout":1000}})]).await;
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
    let content = page.response_body(response_id).await?;
    let content = String::from_utf8_lossy(&content);
    let status = response.status.unwrap_or(200);
    if status >= 400 {
        if let Some(error) = http_error(status, &content) {
            return Ok(AdapterOutput::normalized_error(error));
        }
    }
    let mut full_text = String::new();
    let mut reasoning = String::new();
    for line in content.lines() {
        if let Some(value) = line.strip_prefix("a0:") {
            if let Ok(part) = serde_json::from_str::<String>(value) {
                full_text.push_str(&part);
            }
        } else if let Some(value) = line.strip_prefix("ag:") {
            if let Ok(part) = serde_json::from_str::<String>(value) {
                reasoning.push_str(&part);
            }
        }
    }
    if full_text.is_empty() {
        return Err(AdapterError::new("未解析到有效文本内容"));
    }
    let mut output = AdapterOutput::text(full_text);
    if !reasoning.trim().is_empty() {
        output = output.with_reasoning(reasoning);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::client::mock::MockPage;
    use std::sync::Arc;

    fn request(model_id: &str) -> GenerateRequest {
        GenerateRequest {
            prompt: "Explain tides".into(),
            model_id: model_id.into(),
            image_paths: vec![],
            reasoning: false,
            wait_timeout_ms: 5,
            adapter_config: json!({}),
        }
    }

    #[test]
    fn manifest_contract_preserves_models_aliases_and_search_routes() {
        assert_eq!(LmArenaText.id(), "lmarena_text");
        assert_eq!(LmArenaText.model_ids().len(), 151);
        assert_eq!(
            MODEL_META
                .iter()
                .find(|(id, _, _)| *id == "mimo-v2-flash-thinking")
                .unwrap()
                .1,
            Some("mimo-v2-flash (thinking)")
        );
        assert!(MODEL_META
            .iter()
            .any(|(id, _, search)| *id == "gpt-5.2-search" && *search));
        assert!(
            !MODEL_META
                .iter()
                .find(|(id, _, _)| *id == "grok-4.20-multi-agent-beta-0309")
                .unwrap()
                .2
        );
    }

    #[tokio::test]
    async fn search_model_routes_to_search_page_and_parses_text_and_reasoning() {
        let page = Arc::new(MockPage::returning([
            json!({"url":TARGET_URL_SEARCH,"status":200}),
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
        *page.body.lock().unwrap() =
            "ag:\"checking sources\"\na0:\"The tide\"\na0:\" rises.\"\n".into();
        let output = LmArenaText
            .generate(page.as_ref(), &request("gpt-5.2-search"))
            .await
            .unwrap();
        assert_eq!(output.text.as_deref(), Some("The tide rises."));
        assert_eq!(output.reasoning.as_deref(), Some("checking sources"));
        let calls = page.calls.lock().unwrap();
        assert!(calls
            .iter()
            .flatten()
            .any(|op| op.get("url").and_then(Value::as_str) == Some(TARGET_URL_SEARCH)));
        assert!(calls
            .iter()
            .flatten()
            .any(|op| op.get("text").and_then(Value::as_str) == Some("Explain tides")));
    }
}
