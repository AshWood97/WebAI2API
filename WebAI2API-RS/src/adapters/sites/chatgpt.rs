//! ChatGPT image generation workflow. All site decisions live in Rust; the
//! browser runtime supplies only generic navigation, locator, event, and
//! download primitives.

use crate::adapters::client::{
    chained, count_result, css, http_error, op, page_error, role, AdapterError, AdapterOutput,
    BrowserEvent, ClientFuture, GenerateRequest, PageClient, SiteAdapter,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const CHATGPT_TARGET_URL: &str = "https://chatgpt.com/images/";
const INPUT_SELECTOR: &str = ".ProseMirror";
pub const CHATGPT_MODELS: &[&str] = &["gpt-image-1.5"];

#[derive(Debug, Default, Clone, Copy)]
pub struct ChatGptImageAdapter;

impl SiteAdapter for ChatGptImageAdapter {
    fn id(&self) -> &'static str {
        "chatgpt"
    }
    fn target_url(&self) -> &'static str {
        CHATGPT_TARGET_URL
    }
    fn model_ids(&self) -> &'static [&'static str] {
        CHATGPT_MODELS
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
        let events = page.poll_events(after).await?;
        for event in events {
            after = after.max(event.sequence);
            if event.event_type == "response" && predicate(&event) {
                return Ok(event);
            }
        }
        call(page, vec![json!({"op":"waitForTimeout", "ms":200})]).await?;
    }
    Err(AdapterError::normalized(
        "等待 ChatGPT 图片生成响应超时",
        "TIMEOUT_ERROR",
        true,
    ))
}

async fn wait_uploads(
    page: &dyn PageClient,
    cursor: u64,
    expected: usize,
    timeout_ms: u64,
) -> Result<(), AdapterError> {
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
                if processed >= expected {
                    return Ok(());
                }
            }
        }
        call(page, vec![json!({"op":"waitForTimeout", "ms":200})]).await?;
    }
    Err(AdapterError::normalized(
        format!("图片上传处理超时 ({processed}/{expected})"),
        "TIMEOUT_ERROR",
        true,
    ))
}

fn conversation_text(body: &str) -> String {
    let mut text = String::new();
    for line in body.lines().filter_map(|line| line.strip_prefix("data: ")) {
        let data: Value = match serde_json::from_str(line.trim()) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if data.pointer("/v/message/channel").and_then(Value::as_str) == Some("final")
            && data
                .pointer("/v/message/author/role")
                .and_then(Value::as_str)
                == Some("assistant")
        {
            if let Some(part) = data
                .pointer("/v/message/content/parts/0")
                .and_then(Value::as_str)
            {
                text = part.to_owned();
            }
        }
        if let Some(patches) = data.get("v").and_then(Value::as_array) {
            for patch in patches {
                if patch.get("o").and_then(Value::as_str) == Some("append")
                    && patch.get("p").and_then(Value::as_str) == Some("/message/content/parts/0")
                {
                    if let Some(value) = patch.get("v").and_then(Value::as_str) {
                        text.push_str(value);
                    }
                }
            }
        }
    }
    text
}

fn refusal_or_limit(body: &str, text: &str) -> Option<AdapterError> {
    if text.is_empty() {
        return None;
    }
    let lower_body = body.to_ascii_lowercase();
    let lower_text = text.to_ascii_lowercase();
    if lower_body.contains("ratelimitexception")
        || lower_body.contains("rate limit")
        || lower_text.contains("limit") && lower_text.contains("reset")
    {
        return Some(AdapterError::normalized(
            format!("触发速率限制: {}", preview(text)),
            "RATE_LIMITED",
            false,
        ));
    }
    let has_generation_marker = lower_body.contains("dalle") || lower_body.contains("file_");
    if !has_generation_marker
        && ["cannot", "can't", "unable", "sorry", "policy", "violat"]
            .iter()
            .any(|needle| lower_text.contains(needle))
    {
        return Some(AdapterError::normalized(
            format!("内容被拒绝: {}", preview(text)),
            "CONTENT_BLOCKED",
            false,
        ));
    }
    None
}

fn preview(text: &str) -> String {
    text.chars().take(200).collect()
}

async fn generate(
    page: &dyn PageClient,
    request: &GenerateRequest,
) -> Result<AdapterOutput, AdapterError> {
    let nav = call(page, vec![json!({"op":"goto", "url":CHATGPT_TARGET_URL, "options":{"waitUntil":"load", "timeout":120000}})])
        .await.map_err(|error| page_error(&error.message).unwrap_or(error))?;
    let status = nav
        .first()
        .and_then(|value| value.get("status"))
        .and_then(Value::as_u64);
    if let Some(status) = status.filter(|status| *status >= 400) {
        return Err(AdapterError::normalized(
            format!("网站无法访问 (HTTP {status})"),
            "HTTP_ERROR",
            status >= 500,
        ));
    }
    let input = wait_for_input(page, 90_000).await?;
    if !request.image_paths.is_empty() {
        let add_button = role("button", "Add files and more");
        let cursor = page.subscribe_responses().await?;
        call(page, vec![json!({
            "op":"filechooser.clickAndSetFiles", "locator":add_button,
            "files":request.image_paths.iter().map(|path| path.to_string_lossy().to_string()).collect::<Vec<_>>(),
            "clickOptions":{"timeout":30000}, "fileOptions":{"timeout":30000}, "options":{"timeout":30000}
        })]).await?;
        wait_uploads(
            page,
            cursor,
            request.image_paths.len(),
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
    let event = wait_response(page, cursor, request.wait_timeout_ms.max(1), |event| {
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
    page.response_wait_finished(response_id, request.wait_timeout_ms.max(1))
        .await?;
    let raw = page.response_body(response_id).await?;
    let body = String::from_utf8_lossy(&raw).into_owned();
    let status = event.status.unwrap_or(200);
    if status != 200 {
        return Err(http_error(status, &body)
            .unwrap_or_else(|| AdapterError::new(format!("API 返回错误: HTTP {status}"))));
    }
    let text = conversation_text(&body);
    if let Some(error) = refusal_or_limit(&body, &text) {
        return Err(error);
    }
    let image_timeout = if body.contains("dalle") || body.contains("file_") {
        120_000
    } else {
        30_000
    };
    let download_event = match wait_response(page, cursor, image_timeout, |event| {
        event
            .url
            .as_deref()
            .is_some_and(|url| url.contains("backend-api/files/download/file_"))
            && event.status == Some(200)
    })
    .await
    {
        Ok(event) => event,
        Err(_error) if !text.trim().is_empty() => {
            return Err(AdapterError::normalized(
                format!("模型返回文本而非图片: {}", preview(&text)),
                "CONTENT_BLOCKED",
                false,
            ));
        }
        Err(error) => return Err(page_error(&error.message).unwrap_or(error)),
    };
    let download_id = download_event
        .response_id
        .as_deref()
        .ok_or_else(|| AdapterError::new("ChatGPT 文件响应缺少 responseId"))?;
    page.response_wait_finished(download_id, 30_000).await?;
    let metadata: Value = serde_json::from_slice(&page.response_body(download_id).await?)
        .map_err(|error| AdapterError::new(format!("解析图片下载信息失败: {error}")))?;
    let file_name = metadata
        .get("file_name")
        .and_then(Value::as_str)
        .unwrap_or("");
    let url = metadata
        .get("download_url")
        .and_then(Value::as_str)
        .filter(|url| !url.is_empty());
    if !file_name.starts_with("user-") || file_name.contains(".part") || url.is_none() {
        if !text.is_empty() {
            return Err(AdapterError::normalized(
                format!("模型返回文本而非图片: {}", preview(&text)),
                "CONTENT_BLOCKED",
                false,
            ));
        }
        return Err(AdapterError::new("未获取到图片下载链接"));
    }
    let url = url.unwrap();
    let output_path = unique_temp_path("webai2api-chatgpt", "bin");
    let failover = request
        .adapter_config
        .pointer("/backend/pool/failover")
        .or_else(|| request.adapter_config.get("failover"));
    let retry_enabled = failover
        .and_then(|config| config.get("imgDlRetry"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let retries = if retry_enabled {
        failover
            .and_then(|config| config.get("imgDlRetryMaxRetries"))
            .and_then(Value::as_u64)
            .unwrap_or(2)
            .clamp(1, 10)
    } else {
        0
    };
    let mut attempt = 0;
    let downloaded = loop {
        match page
            .download_fetch_info(url, &output_path, json!({}), 120_000)
            .await
        {
            Ok(result) => break result,
            Err(error) if attempt < retries => {
                if !is_retryable_download_error(&error.message) {
                    return Err(AdapterError::new(format!(
                        "已获取结果，但图片下载时遇到错误: {}",
                        error.message
                    )));
                }
                attempt += 1;
                call(
                    page,
                    vec![json!({"op":"waitForTimeout", "ms":1000u64 * attempt})],
                )
                .await?;
                let _ = tokio::fs::remove_file(&output_path).await;
            }
            Err(error) => {
                return Err(AdapterError::new(format!(
                    "已获取结果，但图片下载时遇到错误: {}",
                    error.message
                )))
            }
        }
    };
    let bytes = tokio::fs::read(&output_path)
        .await
        .map_err(|error| AdapterError::new(format!("读取图片失败: {error}")))?;
    let _ = tokio::fs::remove_file(&output_path).await;
    let mime = downloaded
        .content_type
        .as_deref()
        .unwrap_or("image/png")
        .split(';')
        .next()
        .unwrap_or("image/png")
        .trim();
    Ok(AdapterOutput::image(format!(
        "data:{mime};base64,{}",
        STANDARD.encode(bytes)
    )))
}

fn is_retryable_download_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("http 5")
        || [
            "timeout",
            "network",
            "econnreset",
            "econnrefused",
            "etimedout",
            "disconnected",
            "tls",
            "socket",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
}

fn unique_temp_path(prefix: &str, extension: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "{prefix}-{}-{}.{}",
        nonce,
        NEXT.fetch_add(1, Ordering::Relaxed),
        extension
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::client::mock::MockPage;

    #[test]
    fn metadata_and_target_match_legacy_manifest() {
        let adapter = ChatGptImageAdapter;
        assert_eq!(adapter.id(), "chatgpt");
        assert_eq!(adapter.target_url(), "https://chatgpt.com/images/");
        assert_eq!(adapter.model_ids(), &["gpt-image-1.5"]);
    }

    #[test]
    fn conversation_sse_final_message_and_patch_are_combined() {
        let body = concat!(
            "data: {\"v\":{\"message\":{\"channel\":\"final\",\"author\":{\"role\":\"assistant\"},\"content\":{\"parts\":[\"Starting\"]}}}}\n",
            "data: {\"v\":[{\"o\":\"append\",\"p\":\"/message/content/parts/0\",\"v\":\" now\"}]}\n",
            "data: [DONE]\n"
        );
        assert_eq!(conversation_text(body), "Starting now");
    }

    #[test]
    fn refusals_and_rate_limits_are_non_retryable() {
        assert_eq!(
            refusal_or_limit("RateLimitException", "Please wait for limit reset")
                .unwrap()
                .retryable,
            Some(false)
        );
        assert_eq!(
            refusal_or_limit("", "Sorry, policy does not allow that")
                .unwrap()
                .code
                .as_deref(),
            Some("CONTENT_BLOCKED")
        );
        assert!(refusal_or_limit("dalle generate", "Sorry, I cannot").is_none());
    }

    #[tokio::test]
    async fn unknown_model_fails_before_any_browser_operation() {
        let page = MockPage::default();
        let request = GenerateRequest {
            prompt: "x".into(),
            model_id: "other".into(),
            image_paths: vec![],
            reasoning: false,
            wait_timeout_ms: 1000,
            adapter_config: Value::Null,
        };
        let output = ChatGptImageAdapter.generate(&page, &request).await.unwrap();
        assert_eq!(output.error.as_deref(), Some("未找到模型配置: other"));
        assert!(page.calls.lock().unwrap().is_empty());
    }
}
