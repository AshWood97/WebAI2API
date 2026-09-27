use super::super::client::{
    chained, count_result, css, http_error, op, page_error, string_result, AdapterError,
    AdapterOutput, ClientFuture, GenerateRequest, PageClient, SiteAdapter,
};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

pub const TARGET_URL: &str = "https://zenmux.ai/settings/chat";
const INPUT: &str = ".chat-input-container textarea";
const SEND: &str = ".input-actions-send button";

#[derive(Debug, Clone, Copy)]
struct Model {
    id: &'static str,
    code_name: &'static str,
    provider: &'static str,
}

const MODELS: &[Model] = &[
    Model {
        id: "gemini-3-flash-preview",
        code_name: "google/gemini-3-flash-preview-free",
        provider: "google-vertex",
    },
    Model {
        id: "mimo-v2-flash",
        code_name: "xiaomi/mimo-v2-flash",
        provider: "xiaomi",
    },
    Model {
        id: "glm-4.6v-flash",
        code_name: "z-ai/glm-4.6v-flash",
        provider: "z-ai",
    },
    Model {
        id: "mistral-large-2512",
        code_name: "mistralai/mistral-large-2512",
        provider: "azure",
    },
    Model {
        id: "deepseek-v3.2",
        code_name: "deepseek/deepseek-chat",
        provider: "deepseek",
    },
    Model {
        id: "deepseek-v3.2-thinking",
        code_name: "deepseek/deepseek-reasoner",
        provider: "deepseek",
    },
    Model {
        id: "grok-4.1-fast",
        code_name: "x-ai/grok-4.1-fast",
        provider: "x-ai",
    },
    Model {
        id: "grok-4.1-fast-non-reasoning",
        code_name: "x-ai/grok-4.1-fast-non-reasoning",
        provider: "x-ai",
    },
    Model {
        id: "gpt-5.1-codex-mini",
        code_name: "openai/gpt-5.1-codex-mini",
        provider: "openai",
    },
    Model {
        id: "ernie-5.0-thinking-preview",
        code_name: "baidu/ernie-5.0-thinking-preview",
        provider: "baidu",
    },
    Model {
        id: "doubao-seed-code",
        code_name: "volcengine/doubao-seed-code",
        provider: "volcengine",
    },
    Model {
        id: "kimi-k2-thinking",
        code_name: "moonshotai/kimi-k2-thinking",
        provider: "moonshotai",
    },
    Model {
        id: "minimax-m2",
        code_name: "minimax/minimax-m2",
        provider: "minimax",
    },
    Model {
        id: "kat-coder-pro-v1",
        code_name: "kuaishou/kat-coder-pro-v1",
        provider: "streamlake",
    },
    Model {
        id: "glm-4.6",
        code_name: "z-ai/glm-4.6",
        provider: "z-ai",
    },
    Model {
        id: "claude-sonnet-4.5",
        code_name: "anthropic/claude-sonnet-4.5",
        provider: "anthropic",
    },
    Model {
        id: "qwen3-max",
        code_name: "qwen/qwen3-max",
        provider: "alibaba",
    },
    Model {
        id: "grok-4-fast",
        code_name: "x-ai/grok-4-fast",
        provider: "x-ai",
    },
    Model {
        id: "grok-4-fast-non-reasoning",
        code_name: "x-ai/grok-4-fast-non-reasoning",
        provider: "x-ai",
    },
    Model {
        id: "grok-code-fast-1",
        code_name: "x-ai/grok-code-fast-1",
        provider: "x-ai",
    },
    Model {
        id: "deepseek-v3.1",
        code_name: "deepseek/deepseek-chat-v3.1",
        provider: "theta",
    },
    Model {
        id: "gpt-5-mini",
        code_name: "openai/gpt-5-mini",
        provider: "openai",
    },
    Model {
        id: "gpt-5-nano",
        code_name: "openai/gpt-5-nano",
        provider: "openai",
    },
    Model {
        id: "glm-4.5-air",
        code_name: "z-ai/glm-4.5-air",
        provider: "z-ai",
    },
    Model {
        id: "gemini-2.5-flash-lite",
        code_name: "google/gemini-2.5-flash-lite",
        provider: "google-vertex",
    },
    Model {
        id: "gemini-2.5-flash",
        code_name: "google/gemini-2.5-flash",
        provider: "google-vertex",
    },
    Model {
        id: "deepseek-r1-0528",
        code_name: "deepseek/deepseek-r1-0528",
        provider: "theta",
    },
    Model {
        id: "claude-sonnet-4",
        code_name: "anthropic/claude-sonnet-4",
        provider: "anthropic",
    },
    Model {
        id: "qwen3-14b",
        code_name: "qwen/qwen3-14b",
        provider: "theta",
    },
    Model {
        id: "o4-mini",
        code_name: "openai/o4-mini",
        provider: "openai",
    },
    Model {
        id: "gpt-4.1-mini",
        code_name: "openai/gpt-4.1-mini",
        provider: "openai",
    },
    Model {
        id: "gpt-4.1-nano",
        code_name: "openai/gpt-4.1-nano",
        provider: "openai",
    },
    Model {
        id: "gemini-2.0-flash-lite",
        code_name: "google/gemini-2.0-flash-lite-001",
        provider: "google-vertex",
    },
    Model {
        id: "claude-3.7-sonnet",
        code_name: "anthropic/claude-3.7-sonnet",
        provider: "anthropic",
    },
    Model {
        id: "gemini-2.0-flash",
        code_name: "google/gemini-2.0-flash",
        provider: "google-vertex",
    },
    Model {
        id: "claude-3.5-sonnet",
        code_name: "anthropic/claude-3.5-sonnet",
        provider: "anthropic",
    },
];
pub const MODEL_IDS: &[&str] = &[
    "gemini-3-flash-preview",
    "mimo-v2-flash",
    "glm-4.6v-flash",
    "mistral-large-2512",
    "deepseek-v3.2",
    "deepseek-v3.2-thinking",
    "grok-4.1-fast",
    "grok-4.1-fast-non-reasoning",
    "gpt-5.1-codex-mini",
    "ernie-5.0-thinking-preview",
    "doubao-seed-code",
    "kimi-k2-thinking",
    "minimax-m2",
    "kat-coder-pro-v1",
    "glm-4.6",
    "claude-sonnet-4.5",
    "qwen3-max",
    "grok-4-fast",
    "grok-4-fast-non-reasoning",
    "grok-code-fast-1",
    "deepseek-v3.1",
    "gpt-5-mini",
    "gpt-5-nano",
    "glm-4.5-air",
    "gemini-2.5-flash-lite",
    "gemini-2.5-flash",
    "deepseek-r1-0528",
    "claude-sonnet-4",
    "qwen3-14b",
    "o4-mini",
    "gpt-4.1-mini",
    "gpt-4.1-nano",
    "gemini-2.0-flash-lite",
    "claude-3.7-sonnet",
    "gemini-2.0-flash",
    "claude-3.5-sonnet",
];

#[derive(Debug, Default, Clone, Copy)]
pub struct ZenmuxAiTextAdapter;

impl SiteAdapter for ZenmuxAiTextAdapter {
    fn id(&self) -> &'static str {
        "zenmux_ai_text"
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
            let Some(model) = MODELS.iter().find(|model| model.id == request.model_id) else {
                return Ok(AdapterOutput::error(format!(
                    "未找到模型配置: {}",
                    request.model_id
                )));
            };
            match generate(page, request, model).await {
                Ok(output) => Ok(output),
                Err(error) => Ok(AdapterOutput::normalized_error(error)),
            }
        })
    }
}

async fn call(page: &dyn PageClient, operations: Vec<Value>) -> Result<Vec<Value>, AdapterError> {
    page.call(operations).await
}

async fn wait_input(page: &dyn PageClient) -> Result<Value, AdapterError> {
    let input = css(INPUT);
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(90) {
        if count_result(
            &call(page, vec![op("locator.count", input.clone())]).await?,
            0,
        ) > 0
        {
            return Ok(input);
        }
        call(page, vec![json!({"op":"waitForTimeout","ms":500})]).await?;
    }
    Err(AdapterError::new(
        "未找到输入框 (.chat-input-container textarea)",
    ))
}

async fn choose_new_chat(page: &dyn PageClient) -> Result<(), AdapterError> {
    let spans = css("span");
    let count = count_result(
        &call(page, vec![op("locator.count", spans.clone())]).await?,
        0,
    );
    for index in 0..count {
        let span = chained(spans.clone(), "nth", Some(index));
        let text = call(page, vec![op("locator.text", span.clone())]).await?;
        if string_result(&text, 0).unwrap_or("").contains("New Chat") {
            let _ = call(page, vec![op("locator.click", span)]).await;
            break;
        }
    }
    Ok(())
}

async fn upload_files(
    page: &dyn PageClient,
    target: &Value,
    paths: &[std::path::PathBuf],
    response_match: &str,
) -> Result<(), AdapterError> {
    call(
        page,
        vec![
            op("locator.click", target.clone()),
            json!({"op":"waitForTimeout","ms":400}),
        ],
    )
    .await?;
    let inputs = css("input[type='file']");
    let count = count_result(
        &call(page, vec![op("locator.count", inputs.clone())]).await?,
        0,
    );
    if count == 0 {
        return Err(AdapterError::new(
            "未找到任何 input[type=\"file\"] 控件,无法上传",
        ));
    }
    let files: Vec<String> = paths
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    let cursor = page.subscribe_responses().await?;
    let mut uploaded = false;
    for index in 0..count {
        let input = chained(inputs.clone(), "nth", Some(index));
        if call(
            page,
            vec![json!({"op":"upload","locator":input,"files":files.clone()})],
        )
        .await
        .is_ok()
        {
            uploaded = true;
            break;
        }
    }
    if !uploaded {
        return Err(AdapterError::new("所有文件控件均无法接受输入"));
    }
    let start = Instant::now();
    let mut after = cursor;
    let mut matched = 0;
    while start.elapsed() < Duration::from_secs(60) && matched < paths.len() {
        for event in page.poll_events(after).await? {
            after = after.max(event.sequence);
            if event.event_type == "response"
                && event.status == Some(200)
                && event
                    .request_method
                    .as_deref()
                    .is_some_and(|method| method.eq_ignore_ascii_case("POST"))
                && event
                    .url
                    .as_deref()
                    .is_some_and(|url| url.contains(response_match))
            {
                matched += 1;
            }
        }
        if matched < paths.len() {
            call(page, vec![json!({"op":"waitForTimeout","ms":200})]).await?;
        }
    }
    Ok(())
}

fn rewrite_request_body(model: &Model, raw: &str) -> Option<String> {
    let mut body = serde_json::from_str::<Value>(raw).ok()?;
    let object = body.as_object_mut()?;
    if object
        .get("model")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty())
    {
        object.insert("model".into(), Value::String(model.code_name.into()));
    }
    if !object.get("provider").is_some_and(Value::is_object) {
        object.insert("provider".into(), json!({}));
    }
    let provider = object.get_mut("provider")?.as_object_mut()?;
    if !provider.get("routing").is_some_and(Value::is_object) {
        provider.insert("routing".into(), json!({}));
    }
    provider
        .get_mut("routing")?
        .as_object_mut()?
        .insert("providers".into(), json!([model.provider]));
    Some(body.to_string())
}

async fn send_and_read_response(
    page: &dyn PageClient,
    model: &Model,
    timeout_ms: u64,
) -> Result<String, AdapterError> {
    let route_id = page.route_install("**/*v1/chat/completions*", 1500).await?;
    let result = async {
        let cursor = page.subscribe_responses().await?;
        call(page, vec![op("locator.click", css(SEND))]).await?;
        let start = Instant::now();
        let mut after = cursor;
        while start.elapsed() < Duration::from_millis(timeout_ms) {
            for event in page.poll_events(after).await? {
                after = after.max(event.sequence);
                if event.event_type == "route" {
                    if let Some(token) = event.route_token.as_deref() {
                        let request = event.request.as_ref().cloned().unwrap_or(Value::Null);
                        let url = request.get("url").and_then(Value::as_str).unwrap_or("");
                        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
                        let data = request
                            .get("postData")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let rewritten = if url.contains("v1/chat/completions")
                            && method.eq_ignore_ascii_case("POST")
                        {
                            rewrite_request_body(model, data)
                        } else {
                            None
                        };
                        let decision = rewritten
                            .map(|post_data| json!({"action":"continue","postData":post_data}))
                            .unwrap_or_else(|| json!({"action":"continue"}));
                        page.route_resolve(token, decision).await?;
                    }
                }
                if event.event_type == "response"
                    && event
                        .request_method
                        .as_deref()
                        .is_some_and(|method| method.eq_ignore_ascii_case("POST"))
                    && event
                        .url
                        .as_deref()
                        .is_some_and(|url| url.contains("v1/chat/completions"))
                {
                    let id = event
                        .response_id
                        .as_deref()
                        .ok_or_else(|| AdapterError::new("Zenmux 响应缺少 responseId"))?;
                    let bytes = page.response_body(id).await?;
                    let body = String::from_utf8_lossy(&bytes);
                    if let Some(error) = http_error(event.status.unwrap_or(200), &body) {
                        return Err(error);
                    }
                    return Ok(body.into_owned());
                }
            }
            call(page, vec![json!({"op":"waitForTimeout","ms":150})]).await?;
        }
        Err(AdapterError::normalized(
            "API_TIMEOUT: 等待 Zenmux 回复超时",
            "TIMEOUT_ERROR",
            true,
        ))
    }
    .await;
    let _ = page.route_remove(&route_id).await;
    result
}

fn parse_sse(body: &str) -> String {
    let mut text = String::new();
    for line in body.lines() {
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        if data.trim() == "[DONE]" {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        if let Some(choices) = value.get("choices").and_then(Value::as_array) {
            for choice in choices {
                if let Some(content) = choice
                    .pointer("/delta/content")
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                {
                    text.push_str(content);
                }
            }
        }
    }
    text
}

async fn generate(
    page: &dyn PageClient,
    request: &GenerateRequest,
    model: &Model,
) -> Result<AdapterOutput, AdapterError> {
    if let Err(error) = call(
        page,
        vec![json!({"op":"goto","url":TARGET_URL,"options":{"waitUntil":"load","timeout":120000}})],
    )
    .await
    {
        return Err(page_error(&error.message).unwrap_or(error));
    }
    let _ = choose_new_chat(page).await;
    let input = wait_input(page).await?;
    if !request.image_paths.is_empty() {
        upload_files(page, &input, &request.image_paths, "oss/upload").await?;
    }
    let input = css(INPUT);
    call(page, vec![op("locator.click", input.clone()), json!({"op":"locator.fill","locator":input,"value":request.prompt,"options":{"timeout":10000}})]).await?;
    let body = send_and_read_response(page, model, request.wait_timeout_ms.max(1)).await?;
    let text = parse_sse(&body);
    if text.is_empty() {
        return Ok(AdapterOutput::error("未解析到有效文本内容"));
    }
    Ok(AdapterOutput::text(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::client::mock::MockPage;

    #[test]
    fn metadata_matches_legacy_model_catalog() {
        assert_eq!(ZenmuxAiTextAdapter.id(), "zenmux_ai_text");
        assert_eq!(TARGET_URL, "https://zenmux.ai/settings/chat");
        assert_eq!(MODELS.len(), 36);
        assert_eq!(MODELS[0].code_name, "google/gemini-3-flash-preview-free");
        assert_eq!(
            MODELS
                .iter()
                .find(|m| m.id == "deepseek-v3.2-thinking")
                .unwrap()
                .provider,
            "deepseek"
        );
    }

    #[test]
    fn route_rewrite_changes_model_and_provider_without_dropping_other_fields() {
        let model = MODELS
            .iter()
            .find(|model| model.id == "deepseek-v3.2-thinking")
            .unwrap();
        let rewritten: Value = serde_json::from_str(
            &rewrite_request_body(
                model,
                r#"{"model":"old","messages":[],"provider":{"order":["x"]}}"#,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(rewritten["model"], "deepseek/deepseek-reasoner");
        assert_eq!(
            rewritten["provider"]["routing"]["providers"],
            json!(["deepseek"])
        );
        assert_eq!(rewritten["provider"]["order"], json!(["x"]));
    }

    #[test]
    fn stream_parser_ignores_reasoning_only_deltas_and_done_marker() {
        let body = "data: {\"choices\":[{\"delta\":{\"reasoning\":\"hidden\"}}]}\ndata: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\ndata: [DONE]\n";
        assert_eq!(parse_sse(body), "hello");
    }

    #[tokio::test]
    async fn input_wait_uses_legacy_selector_over_mock_rpc() {
        let page = MockPage::returning([Value::from(1)]);
        let input = wait_input(&page).await.unwrap();
        assert_eq!(input["value"], INPUT);
        assert_eq!(page.calls.lock().unwrap()[0][0]["op"], "locator.count");
    }

    #[tokio::test]
    async fn unsupported_model_fails_before_page_calls() {
        let page = MockPage::default();
        let request = GenerateRequest {
            prompt: "x".into(),
            model_id: "nope".into(),
            image_paths: vec![],
            reasoning: false,
            wait_timeout_ms: 50,
            adapter_config: Value::Null,
        };
        assert_eq!(
            ZenmuxAiTextAdapter
                .generate(&page, &request)
                .await
                .unwrap()
                .error
                .as_deref(),
            Some("未找到模型配置: nope")
        );
        assert!(page.calls.lock().unwrap().is_empty());
    }
}
