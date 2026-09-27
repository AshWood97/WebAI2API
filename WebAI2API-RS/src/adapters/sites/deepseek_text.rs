use super::super::client::{
    chained, count_result, css, http_error, op, page_error, role, string_result, AdapterError,
    AdapterOutput, BrowserEvent, ClientFuture, GenerateRequest, PageClient, SiteAdapter,
};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

pub const TARGET_URL: &str = "https://chat.deepseek.com/";
const INPUT: &str = "textarea";
pub const MODEL_IDS: &[&str] = &[
    "deepseek",
    "deepseek-thinking",
    "deepseek-search",
    "deepseek-thinking-search",
    "deepseek-expert",
    "deepseek-thinking-expert",
    "deepseek-search-expert",
    "deepseek-thinking-search-expert",
];

#[derive(Debug, Default, Clone, Copy)]
pub struct DeepSeekTextAdapter;

impl SiteAdapter for DeepSeekTextAdapter {
    fn id(&self) -> &'static str {
        "deepseek_text"
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
            if !MODEL_IDS.contains(&request.model_id.as_str()) {
                return Ok(AdapterOutput::error(format!(
                    "未找到模型配置: {}",
                    request.model_id
                )));
            }
            if !request.image_paths.is_empty() {
                return Ok(AdapterOutput::error("DeepSeek 文本适配器不支持图片输入"));
            }
            match generate(page, request).await {
                Ok(output) => Ok(output),
                Err(error) => Ok(AdapterOutput::normalized_error(error)),
            }
        })
    }
}

async fn call(page: &dyn PageClient, operations: Vec<Value>) -> Result<Vec<Value>, AdapterError> {
    page.call(operations).await
}

async fn wait_input(page: &dyn PageClient, timeout_ms: u64) -> Result<Value, AdapterError> {
    let locator = css(INPUT);
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(timeout_ms) {
        if count_result(
            &call(page, vec![op("locator.count", locator.clone())]).await?,
            0,
        ) > 0
        {
            return Ok(locator);
        }
        call(page, vec![json!({"op":"waitForTimeout", "ms":500})]).await?;
    }
    Err(AdapterError::new("未找到输入框 (textarea)"))
}

async fn configure(page: &dyn PageClient, model: &str) -> Result<(), AdapterError> {
    let thinking = model.contains("thinking");
    let search = model.contains("search");
    // The legacy UI's toggle check uses element.classList; `class` is its
    // generic DOM equivalent and keeps feature state decisions in Rust.
    for (name, target) in [("DeepThink", thinking), ("Search", search)] {
        let button = role("button", name);
        let count = count_result(
            &call(page, vec![op("locator.count", button.clone())]).await?,
            0,
        );
        if count == 0 {
            continue;
        }
        let values = call(page, vec![json!({"op":"locator.attribute","locator":chained(button.clone(), "first", None),"name":"class"})]).await?;
        let class = string_result(&values, 0).unwrap_or("");
        if class.contains("ds-toggle-button--selected") != target {
            call(
                page,
                vec![
                    op("locator.click", chained(button, "first", None)),
                    json!({"op":"waitForTimeout","ms":350}),
                ],
            )
            .await?;
        }
        call(page, vec![json!({"op":"waitForTimeout","ms":300})]).await?;
    }
    Ok(())
}

fn append_fragment(
    fragment: &Value,
    collecting: &mut bool,
    thinking: &mut bool,
    text: &mut String,
    reasoning: &mut String,
) {
    match fragment.get("type").and_then(Value::as_str) {
        Some("RESPONSE") => {
            *collecting = true;
            *thinking = false;
            if let Some(s) = fragment.get("content").and_then(Value::as_str) {
                text.push_str(s);
            }
        }
        Some("THINK") => {
            *collecting = false;
            *thinking = true;
            if let Some(s) = fragment.get("content").and_then(Value::as_str) {
                reasoning.push_str(s);
            }
        }
        _ => {
            *collecting = false;
            *thinking = false;
        }
    }
}

fn parse_sse(body: &str) -> (String, String, bool) {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut collecting = false;
    let mut thinking = false;
    let mut finished = false;
    for line in body.lines().map(str::trim) {
        let Some(raw) = line.strip_prefix("data:") else {
            continue;
        };
        let raw = raw.trim();
        if raw.is_empty() || raw == "{}" {
            continue;
        }
        let Ok(data) = serde_json::from_str::<Value>(raw) else {
            continue;
        };
        if let Some(fragments) = data
            .pointer("/v/response/fragments")
            .and_then(Value::as_array)
        {
            for fragment in fragments {
                append_fragment(
                    fragment,
                    &mut collecting,
                    &mut thinking,
                    &mut text,
                    &mut reasoning,
                );
            }
        }
        if data.get("p").and_then(Value::as_str) == Some("response/fragments")
            && data.get("o").and_then(Value::as_str) == Some("APPEND")
        {
            if let Some(fragments) = data.get("v").and_then(Value::as_array) {
                for fragment in fragments {
                    append_fragment(
                        fragment,
                        &mut collecting,
                        &mut thinking,
                        &mut text,
                        &mut reasoning,
                    );
                }
            }
        }
        if data.get("o").and_then(Value::as_str) == Some("BATCH")
            && data.get("p").and_then(Value::as_str) == Some("response")
        {
            if let Some(items) = data.get("v").and_then(Value::as_array) {
                for item in items {
                    if item.get("p").and_then(Value::as_str) == Some("fragments")
                        && item.get("o").and_then(Value::as_str) == Some("APPEND")
                    {
                        if let Some(fragments) = item.get("v").and_then(Value::as_array) {
                            for fragment in fragments {
                                append_fragment(
                                    fragment,
                                    &mut collecting,
                                    &mut thinking,
                                    &mut text,
                                    &mut reasoning,
                                );
                            }
                        }
                    }
                    if ["status", "quasi_status"]
                        .contains(&item.get("p").and_then(Value::as_str).unwrap_or(""))
                        && item.get("v").and_then(Value::as_str) == Some("FINISHED")
                    {
                        finished = true;
                    }
                }
            }
        }
        if let Some(path) = data.get("p").and_then(Value::as_str) {
            if path.contains("response/fragments/") && path.ends_with("/content") {
                if let Some(value) = data.get("v").and_then(Value::as_str) {
                    if collecting {
                        text.push_str(value);
                    } else if thinking {
                        reasoning.push_str(value);
                    }
                }
            }
        } else if data.get("o").is_none() {
            if let Some(value) = data.get("v").and_then(Value::as_str) {
                if collecting {
                    text.push_str(value);
                } else if thinking {
                    reasoning.push_str(value);
                }
            }
        }
        if data.get("p").and_then(Value::as_str) == Some("response/status")
            && data.get("o").and_then(Value::as_str) == Some("SET")
            && data.get("v").and_then(Value::as_str) == Some("FINISHED")
        {
            finished = true;
        }
    }
    (
        text.trim().to_owned(),
        reasoning.trim().to_owned(),
        finished,
    )
}

async fn wait_completion(
    page: &dyn PageClient,
    cursor: u64,
    timeout_ms: u64,
) -> Result<(BrowserEvent, String), AdapterError> {
    let start = Instant::now();
    let mut after = cursor;
    while start.elapsed() < Duration::from_millis(timeout_ms) {
        for event in page.poll_events(after).await? {
            after = after.max(event.sequence);
            if event.event_type == "response"
                && event.request_method.as_deref() == Some("POST")
                && event
                    .url
                    .as_deref()
                    .is_some_and(|url| url.contains("chat/completion"))
            {
                let id = event
                    .response_id
                    .as_deref()
                    .ok_or_else(|| AdapterError::new("DeepSeek 响应缺少 responseId"))?;
                let body = page.response_body(id).await?;
                let body = String::from_utf8_lossy(&body);
                if let Some(error) = http_error(event.status.unwrap_or(200), &body) {
                    return Err(error);
                }
                let (_, _, finished) = parse_sse(&body);
                if finished {
                    return Ok((event, body.into_owned()));
                }
            }
        }
        call(page, vec![json!({"op":"waitForTimeout","ms":250})]).await?;
    }
    Err(AdapterError::normalized(
        "API_TIMEOUT: 等待 DeepSeek 回复超时",
        "TIMEOUT_ERROR",
        true,
    ))
}

async fn generate(
    page: &dyn PageClient,
    request: &GenerateRequest,
) -> Result<AdapterOutput, AdapterError> {
    if let Err(error) = call(
        page,
        vec![json!({"op":"goto","url":TARGET_URL,"options":{"waitUntil":"load","timeout":120000}})],
    )
    .await
    {
        return Err(page_error(&error.message).unwrap_or(error));
    }
    let input = wait_input(page, 90_000).await?;
    let expert = request.model_id.ends_with("-expert");
    let mode = if expert { "expert" } else { "default" };
    let mode_button = css(&format!("div[data-model-type='{mode}']"));
    if count_result(
        &call(page, vec![op("locator.count", mode_button.clone())]).await?,
        0,
    ) > 0
    {
        let _ = call(
            page,
            vec![
                op("locator.click", mode_button),
                json!({"op":"waitForTimeout","ms":400}),
            ],
        )
        .await;
    }
    configure(page, &request.model_id).await?;
    call(page, vec![op("locator.click", input.clone()), json!({"op":"locator.fill","locator":input,"value":request.prompt,"options":{"timeout":10000}}), json!({"op":"waitForTimeout","ms":400})]).await?;
    let cursor = page.subscribe_responses().await?;
    call(page, vec![json!({"op":"keyboard.press","key":"Enter"})]).await?;
    let (_, body) = wait_completion(page, cursor, request.wait_timeout_ms.max(1)).await?;
    let (text, reasoning, finished) = parse_sse(&body);
    if !finished {
        return Err(AdapterError::normalized(
            "API_TIMEOUT: DeepSeek 响应未完成",
            "TIMEOUT_ERROR",
            true,
        ));
    }
    if text.is_empty() {
        return Ok(AdapterOutput::error("回复内容为空"));
    }
    let output = AdapterOutput::text(text);
    Ok(if reasoning.is_empty() {
        output
    } else {
        output.with_reasoning(reasoning)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::client::mock::MockPage;

    #[test]
    fn metadata_matches_legacy_models_and_modes() {
        assert_eq!(DeepSeekTextAdapter.id(), "deepseek_text");
        assert_eq!(DeepSeekTextAdapter.target_url(), TARGET_URL);
        assert_eq!(MODEL_IDS.len(), 8);
        assert!(MODEL_IDS.contains(&"deepseek-thinking-search-expert"));
    }

    #[test]
    fn sse_extracts_response_thinking_and_finish_from_multiple_wire_shapes() {
        let body = concat!(
            "data: {\"v\":{\"response\":{\"fragments\":[{\"type\":\"THINK\",\"content\":\"plan\"},{\"type\":\"RESPONSE\",\"content\":\"hello\"}]}}}\n",
            "data: {\"p\":\"response/fragments/-1/content\",\"v\":\" world\"}\n",
            "data: {\"p\":\"response/status\",\"o\":\"SET\",\"v\":\"FINISHED\"}\n"
        );
        assert_eq!(parse_sse(body), ("hello world".into(), "plan".into(), true));
    }

    #[tokio::test]
    async fn input_wait_uses_legacy_selector_over_mock_rpc() {
        let page = MockPage::returning([Value::from(1)]);
        let input = wait_input(&page, 50).await.unwrap();
        assert_eq!(input["value"], INPUT);
        assert_eq!(page.calls.lock().unwrap()[0][0]["op"], "locator.count");
    }

    #[tokio::test]
    async fn rejects_unknown_models_and_image_input_before_browser_calls() {
        for (model, images, expected) in [
            ("missing", vec![], "未找到模型配置: missing"),
            (
                "deepseek",
                vec!["x.png".into()],
                "DeepSeek 文本适配器不支持图片输入",
            ),
        ] {
            let page = MockPage::default();
            let request = GenerateRequest {
                prompt: "x".into(),
                model_id: model.into(),
                image_paths: images,
                reasoning: false,
                wait_timeout_ms: 50,
                adapter_config: Value::Null,
            };
            assert_eq!(
                DeepSeekTextAdapter
                    .generate(&page, &request)
                    .await
                    .unwrap()
                    .error
                    .as_deref(),
                Some(expected)
            );
            assert!(page.calls.lock().unwrap().is_empty());
        }
    }
}
