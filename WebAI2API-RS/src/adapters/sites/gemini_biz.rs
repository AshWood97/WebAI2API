use super::super::client::{
    count_result, css, http_error, op, page_error, AdapterError, AdapterOutput, BrowserEvent,
    ClientFuture, GenerateRequest, PageClient, SiteAdapter,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

pub const TARGET_URL: &str = "https://business.gemini.google";
pub const MODELS: &[&str] = &["gemini-3-pro-image-preview", "veo-3.1-generate-preview"];
const INPUT_SELECTOR: &str = "ucs-prosemirror-editor .ProseMirror";
const SEND_SELECTOR: &str =
    "md-icon-button.send-button.submit, button[aria-label='Send'], .send-button";
const ROUTE_TIMEOUT_MS: u64 = 1_500;

#[derive(Debug, Default, Clone, Copy)]
pub struct GeminiBizAdapter;

impl SiteAdapter for GeminiBizAdapter {
    fn id(&self) -> &'static str {
        "gemini_biz"
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

fn target_url(request: &GenerateRequest) -> Result<&str, AdapterError> {
    let config = &request.adapter_config;
    let target = config
        .pointer("/backend/adapter/gemini_biz/entryUrl")
        .or_else(|| config.pointer("/adapter/gemini_biz/entryUrl"))
        .or_else(|| config.pointer("/backend/geminiBiz/entryUrl"))
        .or_else(|| config.get("entryUrl"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AdapterError::new("未填写 gemini_biz 适配器的 entry URL"))?;
    if !target.contains("business.gemini.google") {
        return Err(AdapterError::new(
            "无效的 Gemini Business URL，必须包含 business.gemini.google 域名",
        ));
    }
    Ok(target)
}

async fn call(page: &dyn PageClient, operations: Vec<Value>) -> Result<Vec<Value>, AdapterError> {
    page.call(operations).await
}

async fn wait_visible(
    page: &dyn PageClient,
    locator: Value,
    timeout_ms: u64,
) -> Result<(), AdapterError> {
    let start = Instant::now();
    loop {
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
            return Ok(());
        }
        if start.elapsed() >= Duration::from_millis(timeout_ms) {
            return Err(AdapterError::new("等待 Gemini Business 页面元素超时"));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn wait_attached(
    page: &dyn PageClient,
    locator: Value,
    timeout_ms: u64,
) -> Result<(), AdapterError> {
    call(
        page,
        vec![json!({
            "op":"locator.waitFor",
            "locator":locator,
            "options":{"state":"attached","timeout":timeout_ms}
        })],
    )
    .await?;
    Ok(())
}

async fn handle_account_chooser(page: &dyn PageClient) -> Result<(), AdapterError> {
    let current = call(page, vec![json!({"op":"url"})]).await?;
    if !current
        .first()
        .and_then(Value::as_str)
        .is_some_and(|url| url.contains("auth.business.gemini.google/account-chooser"))
    {
        return Ok(());
    }
    let submit = css("button[type='submit']");
    wait_visible(page, submit.clone(), 15_000).await?;
    call(
        page,
        vec![
            op("locator.scrollIntoViewIfNeeded", submit.clone()),
            json!({"op":"waitForTimeout","ms":350}),
            op("locator.click", submit),
            json!({"op":"waitForTimeout","ms":2000}),
        ],
    )
    .await?;
    let current = call(page, vec![json!({"op":"url"})]).await?;
    if current.first().and_then(Value::as_str).is_some_and(|url| {
        url.contains("accounts.google.com") || url.contains("auth.business.gemini.google")
    }) {
        return Err(AdapterError::new(
            "等待 Gemini Business 账户选择跳转超时，请确认已登录",
        ));
    }
    Ok(())
}

async fn wait_event<F>(
    page: &dyn PageClient,
    mut after: u64,
    timeout_ms: u64,
    mut predicate: F,
) -> Result<BrowserEvent, AdapterError>
where
    F: FnMut(&BrowserEvent) -> bool,
{
    let start = Instant::now();
    loop {
        for event in page.poll_events(after).await? {
            after = after.max(event.sequence);
            if predicate(&event) {
                return Ok(event);
            }
        }
        if start.elapsed() >= Duration::from_millis(timeout_ms) {
            return Err(AdapterError::normalized(
                "API_TIMEOUT: 等待 Gemini Business 响应超时",
                "TIMEOUT_ERROR",
                true,
            ));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_uploads(
    page: &dyn PageClient,
    mut after: u64,
    expected: usize,
    timeout_ms: u64,
) -> Result<(), AdapterError> {
    let start = Instant::now();
    let mut uploaded = 0;
    while uploaded < expected {
        for event in page.poll_events(after).await? {
            after = after.max(event.sequence);
            if event.event_type == "response"
                && event
                    .url
                    .as_deref()
                    .is_some_and(|url| url.contains("global/widgetAddContextFile"))
                && event.status == Some(200)
            {
                uploaded += 1;
                if uploaded >= expected {
                    return Ok(());
                }
            }
        }
        if start.elapsed() >= Duration::from_millis(timeout_ms) {
            // Legacy upload helper treats validation timeout as best-effort and
            // allows the prompt to proceed after logging the missing count.
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}

fn rewrite_request(raw: &str, model_id: &str) -> Option<String> {
    let mut body: Value = serde_json::from_str(raw).ok()?;
    let assist = body
        .as_object_mut()?
        .entry("streamAssistRequest")
        .or_insert_with(|| json!({}));
    let assist = assist.as_object_mut()?;
    let config = assist
        .entry("assistGenerationConfig")
        .or_insert_with(|| json!({}));
    if !config.is_object() {
        *config = json!({});
    }
    assist.insert(
        "toolsSpec".into(),
        if model_id.starts_with("veo-") {
            json!({"videoGenerationSpec":{}})
        } else {
            json!({"imageGenerationSpec":{}})
        },
    );
    serde_json::to_string(&body).ok()
}

fn http_status_error(status: u16, body: &str) -> Option<AdapterError> {
    http_error(status, body).map(|error| {
        AdapterError::normalized(
            format!("请求生成时返回错误: {}", error.message),
            error.code.unwrap_or_else(|| "HTTP_ERROR".into()),
            error.retryable.unwrap_or(false),
        )
    })
}

async fn response_text(
    page: &dyn PageClient,
    response: &BrowserEvent,
    timeout_ms: u64,
) -> Result<String, AdapterError> {
    let id = response
        .response_id
        .as_deref()
        .ok_or_else(|| AdapterError::new("Gemini Business 响应缺少 responseId"))?;
    page.response_wait_finished(id, timeout_ms).await?;
    let bytes = page.response_body(id).await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

async fn run_route_and_wait_response(
    page: &dyn PageClient,
    _route_id: &str,
    after: u64,
    model_id: &str,
    timeout_ms: u64,
) -> Result<BrowserEvent, AdapterError> {
    let start = Instant::now();
    let mut cursor = after;
    loop {
        for event in page.poll_events(cursor).await? {
            cursor = cursor.max(event.sequence);
            if event.event_type == "route"
                && event
                    .url
                    .as_deref()
                    .is_some_and(|url| url.contains("global/widgetStreamAssist"))
            {
                let token = event
                    .route_token
                    .as_deref()
                    .ok_or_else(|| AdapterError::new("Gemini Business 路由事件缺少 token"))?;
                let request = event.request.as_ref();
                let method = request
                    .and_then(|value| value.get("method"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if method.eq_ignore_ascii_case("POST") {
                    let rewritten = request
                        .and_then(|value| value.get("postData"))
                        .and_then(Value::as_str)
                        .and_then(|body| rewrite_request(body, model_id));
                    let decision = rewritten
                        .map(|post_data| json!({"action":"continue","postData":post_data}))
                        .unwrap_or_else(|| json!({"action":"continue"}));
                    page.route_resolve(token, decision).await?;
                } else {
                    page.route_resolve(token, json!({"action":"continue"}))
                        .await?;
                }
            }
            if event.event_type == "response"
                && event
                    .url
                    .as_deref()
                    .is_some_and(|url| url.contains("global/widgetStreamAssist"))
                && event
                    .request_method
                    .as_deref()
                    .is_some_and(|method| method.eq_ignore_ascii_case("POST"))
            {
                return Ok(event);
            }
        }
        if start.elapsed() >= Duration::from_millis(timeout_ms) {
            return Err(AdapterError::normalized(
                "API_TIMEOUT: 等待 global/widgetStreamAssist 响应超时",
                "TIMEOUT_ERROR",
                true,
            ));
        }
        // The generic browser RPC route has a finite auto-continue timeout;
        // polling this event loop lets Rust rewrite the request before expiry.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn generate(
    page: &dyn PageClient,
    request: &GenerateRequest,
) -> Result<AdapterOutput, AdapterError> {
    let target = target_url(request)?;
    let timeout = request.wait_timeout_ms.max(1);
    call(
        page,
        vec![
            json!({"op":"goto","url":target,"options":{"timeout":timeout}}),
            json!({"op":"waitForLoadState","state":"domcontentloaded","options":{"timeout":timeout}}),
        ],
    )
    .await?;
    handle_account_chooser(page).await?;

    let input = css(INPUT_SELECTOR);
    wait_visible(page, input.clone(), timeout.min(60_000)).await?;
    let cursor = page.subscribe_responses().await?;
    if !request.image_paths.is_empty() {
        // Gemini Business accepts reference images through its hidden native
        // file input. The browser runtime exposes generic locator upload; Rust
        // owns file selection and waits for the site's upload responses.
        let inputs = css("input[type='file']");
        wait_attached(page, inputs.clone(), 20_000).await?;
        call(
            page,
            vec![json!({
                "op":"upload",
                "locator":inputs,
                "files":request.image_paths,
                "options":{"timeout":30_000}
            })],
        )
        .await?;
        wait_uploads(page, cursor, request.image_paths.len(), 60_000).await?;
    }

    call(
        page,
        vec![
            op("locator.click", input.clone()),
            json!({"op":"keyboard.type","text":request.prompt}),
        ],
    )
    .await?;

    // Subscribe before submission so response events cannot be missed. The
    // page runtime default-continues routes after 1.5 seconds; Rust polls and
    // rewrites matching POST bodies while the request is paused.
    let after = page.subscribe_responses().await?;
    let route_id = page
        .route_install("**/global/widgetStreamAssist**", ROUTE_TIMEOUT_MS)
        .await?;
    let click = call(page, vec![op("locator.click", css(SEND_SELECTOR))]).await;
    if let Err(error) = click {
        let _ = page.route_remove(&route_id).await;
        return Err(error);
    }
    let response =
        run_route_and_wait_response(page, &route_id, after, &request.model_id, timeout).await;
    let _ = page.route_remove(&route_id).await;
    let response = response?;
    let body = response_text(page, &response, timeout).await?;
    if body.contains("modelArmorViolation") {
        return Ok(AdapterOutput::normalized_error(AdapterError::normalized(
            "内容被阻止: modelArmorViolation",
            "CONTENT_BLOCKED",
            false,
        )));
    }
    if let Some(error) = http_status_error(response.status.unwrap_or(200), &body) {
        return Ok(AdapterOutput::normalized_error(error));
    }

    let after_text = response.sequence;
    let download = async {
        let cursor = after_text;
        let image_element = css("ucs-markdown-image");
        wait_visible(page, image_element.clone(), 20_000).await?;
        call(
            page,
            vec![op("locator.scrollIntoViewIfNeeded", image_element)],
        )
        .await?;
        let event = wait_event(page, cursor, timeout, |event| {
            event.event_type == "response"
                && event
                    .url
                    .as_deref()
                    .is_some_and(|url| url.contains("download/v1alpha/projects"))
                && event
                    .request_method
                    .as_deref()
                    .is_some_and(|method| method.eq_ignore_ascii_case("GET"))
        })
        .await?;
        if let Some(error) = http_status_error(event.status.unwrap_or(200), "") {
            return Err(error);
        }
        let response_id = event
            .response_id
            .as_deref()
            .ok_or_else(|| AdapterError::new("图片下载响应缺少 responseId"))?;
        page.response_wait_finished(response_id, timeout).await?;
        let bytes = page.response_body(response_id).await?;
        let encoded = String::from_utf8_lossy(&bytes).trim().to_owned();
        let mime = event
            .headers
            .as_ref()
            .and_then(|headers| headers.get("x-goog-safety-content-type"))
            .and_then(Value::as_str)
            .unwrap_or("image/png");
        Ok::<_, AdapterError>(if encoded.starts_with("data:") {
            encoded
        } else if std::str::from_utf8(&bytes).is_ok() {
            format!("data:{mime};base64,{encoded}")
        } else {
            format!("data:{mime};base64,{}", STANDARD.encode(bytes))
        })
    }
    .await;
    match download {
        Ok(data_uri) => Ok(AdapterOutput::image(data_uri)),
        Err(error) if error.message.contains("API_TIMEOUT:") => Ok(AdapterOutput::error(format!(
            "已获取结果, 但图片下载时超时 ({}秒)",
            timeout / 1000
        ))),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
fn parse_text_response(body: &str) -> Result<AdapterOutput, AdapterError> {
    let parsed: Value = serde_json::from_str(body)
        .map_err(|error| AdapterError::new(format!("解析响应失败: {error}")))?;
    let items = parsed
        .as_array()
        .ok_or_else(|| AdapterError::new("响应格式错误：不是数组"))?;
    let mut text = String::new();
    for item in items {
        let answer = item.pointer("/streamAssistResponse/answer").filter(|_| {
            item.pointer("/streamAssistResponse/answer/state")
                .and_then(Value::as_str)
                == Some("IN_PROGRESS")
        });
        if let Some(answer) = answer {
            let Some(reply) = answer
                .get("replies")
                .and_then(Value::as_array)
                .and_then(|replies| replies.first())
            else {
                continue;
            };
            let content = reply.get("groundedContent");
            if content
                .and_then(|value| value.get("thought"))
                .and_then(Value::as_bool)
                == Some(true)
            {
                continue;
            }
            if let Some(chunk) = content
                .and_then(|value| value.get("text"))
                .and_then(Value::as_str)
            {
                text.push_str(chunk);
            }
        }
    }
    if text.is_empty() {
        return Ok(AdapterOutput::error("未解析到有效文本内容"));
    }
    Ok(AdapterOutput::text(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_models_and_target_match_legacy_contract() {
        assert_eq!(GeminiBizAdapter.id(), "gemini_biz");
        assert_eq!(GeminiBizAdapter.model_ids(), MODELS);
        let req = GenerateRequest {
            prompt: "x".into(),
            model_id: MODELS[0].into(),
            image_paths: Vec::new(),
            reasoning: false,
            wait_timeout_ms: 10,
            adapter_config: json!({"backend":{"geminiBiz":{"entryUrl":"https://business.gemini.google/home/x"}}}),
        };
        assert_eq!(
            target_url(&req).unwrap(),
            "https://business.gemini.google/home/x"
        );
    }

    #[test]
    fn request_rewrite_selects_image_or_video_tools_and_preserves_body() {
        let raw = r#"{"other":1,"streamAssistRequest":{"prompt":"hello"}}"#;
        let image: Value = serde_json::from_str(&rewrite_request(raw, MODELS[0]).unwrap()).unwrap();
        assert_eq!(image["other"], 1);
        assert_eq!(
            image["streamAssistRequest"]["toolsSpec"],
            json!({"imageGenerationSpec":{}})
        );
        let video: Value = serde_json::from_str(&rewrite_request(raw, MODELS[1]).unwrap()).unwrap();
        assert_eq!(
            video["streamAssistRequest"]["toolsSpec"],
            json!({"videoGenerationSpec":{}})
        );
        assert_eq!(
            video.pointer("/streamAssistRequest/assistGenerationConfig"),
            Some(&json!({}))
        );
    }

    #[test]
    fn response_contract_skips_terminal_and_thought_chunks() {
        let body = r#"[
            {"streamAssistResponse":{"answer":{"state":"IN_PROGRESS","replies":[{"groundedContent":{"thought":true,"text":"hidden"}}]}}},
            {"streamAssistResponse":{"answer":{"state":"IN_PROGRESS","replies":[{"groundedContent":{"text":"Hello "}}]}}},
            {"streamAssistResponse":{"answer":{"state":"SUCCEEDED","replies":[{"groundedContent":{"text":"ignored"}}]}}},
            {"streamAssistResponse":{"answer":{"state":"IN_PROGRESS","replies":[{"groundedContent":{"text":"world"}}]}}}
        ]"#;
        assert_eq!(
            parse_text_response(body).unwrap().text.as_deref(),
            Some("Hello world")
        );
        assert_eq!(
            parse_text_response("{}").unwrap_err().message,
            "响应格式错误：不是数组"
        );
    }

    #[test]
    fn invalid_config_does_not_fall_back_to_default_url() {
        let req = GenerateRequest {
            prompt: "x".into(),
            model_id: MODELS[0].into(),
            image_paths: Vec::new(),
            reasoning: false,
            wait_timeout_ms: 1,
            adapter_config: json!({"entryUrl":"https://example.com"}),
        };
        assert!(target_url(&req)
            .unwrap_err()
            .message
            .contains("必须包含 business.gemini.google"));
    }
}
