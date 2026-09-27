use super::super::client::{
    chained, count_result, css, http_error, op, page_error, AdapterError, AdapterOutput,
    BrowserEvent, ClientFuture, GenerateRequest, PageClient, SiteAdapter,
};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

pub const TARGET_URL: &str = "https://nanobananafree.ai/";
pub const MODELS: &[&str] = &["gemini-2.5-flash-image"];
const INPUT: &str = "textarea";
#[derive(Debug, Default, Clone, Copy)]
pub struct NanoBananaFreeAdapter;

impl SiteAdapter for NanoBananaFreeAdapter {
    fn id(&self) -> &'static str {
        "nanobananafree_ai"
    }
    fn target_url(&self) -> &'static str {
        TARGET_URL
    }
    fn model_ids(&self) -> &'static [&'static str] {
        MODELS
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
                Err(error) => Ok(AdapterOutput::normalized_error(
                    page_error(&error.message).unwrap_or(error),
                )),
            }
        })
    }
}

async fn call(page: &dyn PageClient, operations: Vec<Value>) -> Result<Vec<Value>, AdapterError> {
    page.call(operations).await
}

async fn wait_visible(
    page: &dyn PageClient,
    locator: Value,
    timeout_ms: u64,
) -> Result<(), AdapterError> {
    let started = Instant::now();
    loop {
        let values = call(page, vec![op("locator.count", locator.clone())]).await?;
        if count_result(&values, 0) > 0 {
            let values = call(page, vec![op("locator.visible", locator.clone())]).await?;
            if values.first().and_then(Value::as_bool).unwrap_or(false) {
                return Ok(());
            }
        }
        if started.elapsed() >= Duration::from_millis(timeout_ms) {
            return Err(AdapterError::new(format!("等待页面元素超时: {INPUT}")));
        }
        call(page, vec![json!({"op":"waitForTimeout","ms":250})]).await?;
    }
}

async fn wait_response(
    page: &dyn PageClient,
    cursor: u64,
    timeout_ms: u64,
) -> Result<BrowserEvent, AdapterError> {
    let started = Instant::now();
    let mut after = cursor;
    while started.elapsed() < Duration::from_millis(timeout_ms) {
        for event in page.poll_events(after).await? {
            after = after.max(event.sequence);
            if event.event_type == "response"
                && event
                    .request_method
                    .as_deref()
                    .is_some_and(|method| method.eq_ignore_ascii_case("POST"))
                && event
                    .url
                    .as_deref()
                    .is_some_and(|url| url.contains("v1/generateContent"))
            {
                return Ok(event);
            }
        }
        call(page, vec![json!({"op":"waitForTimeout","ms":200})]).await?;
    }
    Err(AdapterError::normalized(
        "API_TIMEOUT: 等待 NanoBananaFree 生成响应超时",
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
    let input = css(INPUT);
    wait_visible(page, input.clone(), 90_000).await?;

    // This adapter accepts a single reference image. Preserve the original
    // behavior by ignoring any additional paths.
    if let Some(path) = request.image_paths.first() {
        let file_input = chained(css("input[type='file']"), "first", None);
        call(
            page,
            vec![
                op("locator.click", input.clone()),
                json!({"op":"upload","locator":file_input,"files":[path]} ),
            ],
        )
        .await?;
    }

    call(
        page,
        vec![
            op("locator.click", input.clone()),
            json!({"op":"locator.fill","locator":input.clone(),"value":request.prompt}),
        ],
    )
    .await?;
    let cursor = page.subscribe_responses().await?;
    let send = css("div[class*='_sendButton_']");
    call(page, vec![op("locator.click", send)]).await?;
    let event = wait_response(page, cursor, request.wait_timeout_ms.max(1)).await?;
    let response_id = event
        .response_id
        .as_deref()
        .ok_or_else(|| AdapterError::new("响应缺少 responseId"))?;
    let body_bytes = page.response_body(response_id).await?;
    let body = String::from_utf8_lossy(&body_bytes);
    if let Some(error) = http_error(event.status.unwrap_or(200), &body) {
        return Err(error);
    }
    let parsed: Value =
        serde_json::from_str(&body).map_err(|_| AdapterError::new("解析响应JSON时出错"))?;
    if let Some(data) = parsed
        .pointer("/data/candidates/0/content/parts/0/inlineData/data")
        .and_then(Value::as_str)
    {
        return Ok(AdapterOutput::image(format!(
            "data:image/png;base64,{data}"
        )));
    }
    Ok(AdapterOutput::text(parsed.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::client::mock::MockPage;

    #[test]
    fn manifest_matches_legacy_contract() {
        assert_eq!(NanoBananaFreeAdapter.id(), "nanobananafree_ai");
        assert_eq!(
            NanoBananaFreeAdapter.target_url(),
            "https://nanobananafree.ai/"
        );
        assert_eq!(
            NanoBananaFreeAdapter.model_ids(),
            &["gemini-2.5-flash-image"]
        );
    }

    #[tokio::test]
    async fn unsupported_model_is_rejected_without_browser_calls() {
        let page = MockPage::default();
        let request = GenerateRequest {
            prompt: "x".into(),
            model_id: "missing".into(),
            image_paths: vec![],
            reasoning: false,
            wait_timeout_ms: 1,
            adapter_config: Value::Null,
        };
        let result = NanoBananaFreeAdapter
            .generate(&page, &request)
            .await
            .unwrap();
        assert_eq!(result.error.as_deref(), Some("未找到模型配置: missing"));
        assert!(page.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn parses_inline_image_from_generate_response() {
        let page = MockPage::returning([
            Value::Null,
            json!(1),
            Value::Bool(true),
            Value::Null,
            Value::Null,
        ]);
        page.events.lock().unwrap().push(BrowserEvent {
            sequence: 1,
            event_type: "response".into(),
            browser_id: None,
            page_id: None,
            generation: None,
            response_id: Some("response-1".into()),
            url: Some("https://x/v1/generateContent".into()),
            status: Some(200),
            request_method: Some("POST".into()),
            headers: None,
            route_token: None,
            request: None,
        });
        *page.body.lock().unwrap() = r#"{"data":{"candidates":[{"content":{"parts":[{"inlineData":{"data":"aGVsbG8="}}]}}]}}"#.into();
        // The mock supplies page-call results in operation order; generation
        // response is immediate after the send click.
        let request = GenerateRequest {
            prompt: "a cat".into(),
            model_id: MODELS[0].into(),
            image_paths: vec![],
            reasoning: false,
            wait_timeout_ms: 100,
            adapter_config: Value::Null,
        };
        let result = NanoBananaFreeAdapter
            .generate(&page, &request)
            .await
            .unwrap();
        assert_eq!(
            result.image.as_deref(),
            Some("data:image/png;base64,aGVsbG8=")
        );
        assert!(result.error.is_none());
    }
}
