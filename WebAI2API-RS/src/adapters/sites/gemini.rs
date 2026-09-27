use super::super::client::{
    chained, count_result, http_error, op, page_error, role, AdapterError, AdapterOutput,
    BrowserEvent, ClientFuture, GenerateRequest, PageClient, SiteAdapter,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

pub const TARGET_URL: &str = "https://gemini.google.com/app?hl=en";
pub const MODEL_IDS: &[&str] = &["gemini-3-pro-image-preview", "veo-3.1-generate-preview"];
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Default, Clone, Copy)]
pub struct GeminiAdapter;

impl SiteAdapter for GeminiAdapter {
    fn id(&self) -> &'static str {
        "gemini"
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
            match generate(page, request).await {
                Ok(output) => Ok(output),
                Err(error) => {
                    let error = page_error(&error.message).unwrap_or(error);
                    Ok(AdapterOutput::normalized_error(error))
                }
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
        if started.elapsed() >= Duration::from_millis(timeout_ms) {
            return Err(AdapterError::new("等待 Gemini 页面元素超时"));
        }
        call(page, vec![json!({"op":"waitForTimeout","ms":200})]).await?;
    }
}

async fn wait_response(
    page: &dyn PageClient,
    cursor: u64,
    url_part: &str,
    method: Option<&str>,
    extra_url_part: Option<&str>,
    timeout_ms: u64,
) -> Result<BrowserEvent, AdapterError> {
    let started = Instant::now();
    let mut after = cursor;
    while started.elapsed() < Duration::from_millis(timeout_ms.max(1)) {
        for event in page.poll_events(after).await? {
            after = after.max(event.sequence);
            if event.event_type == "page.closed" {
                return Err(AdapterError::new("PAGE_CLOSED"));
            }
            let Some(url) = event.url.as_deref() else {
                continue;
            };
            if event.event_type == "response"
                && url.contains(url_part)
                && extra_url_part.is_none_or(|part| url.contains(part))
                && method.is_none_or(|expected| {
                    event
                        .request_method
                        .as_deref()
                        .is_some_and(|actual| actual.eq_ignore_ascii_case(expected))
                })
            {
                return Ok(event);
            }
        }
        call(page, vec![json!({"op":"waitForTimeout","ms":200})]).await?;
    }
    Err(AdapterError::normalized(
        format!("API_TIMEOUT: 等待 {url_part} 响应超时"),
        "TIMEOUT_ERROR",
        true,
    ))
}

async fn upload_file(
    page: &dyn PageClient,
    path: &std::path::Path,
    timeout_ms: u64,
) -> Result<(), AdapterError> {
    let open = role("button", "Open upload file menu");
    wait_visible(page, open.clone(), 15_000).await?;
    call(page, vec![op("locator.click", open)]).await?;
    let upload = role("menuitem", "Upload files");
    wait_visible(page, upload.clone(), 15_000).await?;
    let cursor = page.subscribe_responses().await?;
    call(
        page,
        vec![json!({
            "op":"filechooser.clickAndSetFiles",
            "locator":upload,
            "files":[path],
            "options":{"timeout":30000}
        })],
    )
    .await?;
    let response = wait_response(
        page,
        cursor,
        "google.com/upload/",
        None,
        Some("upload_id="),
        timeout_ms,
    )
    .await?;
    if let Some(id) = response.response_id.as_deref() {
        page.response_wait_finished(id, timeout_ms.max(1)).await?;
    }
    if response.status != Some(200) {
        return Err(http_error(response.status.unwrap_or(500), "upload failed")
            .unwrap_or_else(|| AdapterError::new("Gemini 图片上传失败")));
    }
    Ok(())
}

async fn click_send(page: &dyn PageClient, input: &Value) -> Result<(), AdapterError> {
    let candidates = [
        role("button", "Send message"),
        super::super::client::css("button[aria-label*='Send'], button[aria-label*='Submit']"),
        json!({"kind":"css","value":"button:has(mat-icon)","chain":[{"op":"filter","hasText":"send"},{"op":"last"}]}),
    ];
    for candidate in candidates {
        let candidate = chained(candidate, "last", None);
        if wait_visible(page, candidate.clone(), 5_000).await.is_ok()
            && call(page, vec![op("locator.enabled", candidate.clone())])
                .await
                .ok()
                .and_then(|result| result.first().and_then(Value::as_bool))
                .unwrap_or(false)
            && call(
                page,
                vec![json!({"op":"locator.click","locator":candidate,"options":{"force":true,"timeout":5000}})],
            )
            .await
            .is_ok()
        {
            return Ok(());
        }
    }
    call(page, vec![op("locator.click", input.clone())]).await?;
    call(page, vec![json!({"op":"keyboard.press","key":"Enter"})]).await?;
    Ok(())
}

async fn enable_creation_mode(page: &dyn PageClient, video: bool) -> Result<(), AdapterError> {
    let tools = role("button", "Tools");
    wait_visible(page, tools.clone(), 15_000).await?;
    call(page, vec![op("locator.click", tools)]).await?;
    let label = if video {
        "Create video"
    } else {
        "Create image"
    };
    let item = role("menuitemcheckbox", label);
    let count = count_result(
        &call(page, vec![op("locator.count", item.clone())]).await?,
        0,
    );
    if video && count == 0 {
        return Err(AdapterError::new(
            "该账号不支持视频生成功能 (未找到 Create video 按钮)",
        ));
    }
    wait_visible(page, item.clone(), 10_000).await?;
    call(page, vec![op("locator.click", item)]).await?;
    Ok(())
}

fn temporary_chat(request: &GenerateRequest) -> bool {
    request
        .adapter_config
        .get("temporaryChat")
        .and_then(Value::as_bool)
        .or_else(|| {
            request
                .adapter_config
                .pointer("/backend/adapter/gemini/temporaryChat")
                .and_then(Value::as_bool)
        })
        .unwrap_or(false)
}

async fn generate(
    page: &dyn PageClient,
    request: &GenerateRequest,
) -> Result<AdapterOutput, AdapterError> {
    call(
        page,
        vec![json!({"op":"goto","url":TARGET_URL,"options":{"waitUntil":"load","timeout":120000}})],
    )
    .await?;
    if temporary_chat(request) {
        let temp = role("button", "Temporary chat");
        if wait_visible(page, temp.clone(), 3_000).await.is_ok() {
            let _ = call(page, vec![op("locator.click", temp)]).await;
        }
    }
    let input = role("textbox", "");
    wait_visible(page, input.clone(), 30_000).await?;
    for path in &request.image_paths {
        upload_file(page, path, request.wait_timeout_ms).await?;
    }

    let video = request.model_id.starts_with("veo-");
    enable_creation_mode(page, video).await?;
    call(
        page,
        vec![
            op("locator.click", input.clone()),
            json!({"op":"locator.fill","locator":input.clone(),"value":request.prompt}),
        ],
    )
    .await?;
    let cursor = page.subscribe_responses().await?;
    click_send(page, &input).await?;
    let response = wait_response(
        page,
        cursor,
        "assistant.lamda.BardFrontendService/StreamGenerate",
        Some("POST"),
        None,
        request.wait_timeout_ms,
    )
    .await?;
    let response_id = response
        .response_id
        .as_deref()
        .ok_or_else(|| AdapterError::new("Gemini 响应缺少 responseId"))?;
    page.response_wait_finished(response_id, request.wait_timeout_ms.max(1))
        .await?;
    let body = page.response_body(response_id).await?;
    let body_text = String::from_utf8_lossy(&body);
    if let Some(error) = http_error(response.status.unwrap_or(200), &body_text) {
        return Err(AdapterError::new(format!(
            "API 返回错误: {}",
            error.message
        )));
    }

    if video {
        let video_response = wait_response(
            page,
            cursor,
            "contribution.usercontent.google.com/download",
            Some("GET"),
            Some("filename=video.mp4"),
            request.wait_timeout_ms,
        )
        .await?;
        let id = video_response
            .response_id
            .as_deref()
            .ok_or_else(|| AdapterError::new("Gemini 视频响应缺少 responseId"))?;
        page.response_wait_finished(id, request.wait_timeout_ms.max(1))
            .await?;
        let bytes = page.response_body(id).await?;
        let mime = video_response
            .headers
            .as_ref()
            .and_then(|headers| headers.get("content-type"))
            .and_then(Value::as_str)
            .unwrap_or("video/mp4");
        return Ok(AdapterOutput::image(format!(
            "data:{mime};base64,{}",
            STANDARD.encode(bytes)
        )));
    }

    let payloads = extract_payloads(&body)?;
    let image_urls = collect_image_urls(&payloads);
    if image_urls.is_empty() {
        let message = collect_best_rc_text(&payloads);
        return Ok(AdapterOutput::error(if message.is_empty() {
            "生成失败，响应中未包含图片".to_owned()
        } else {
            message.chars().take(150).collect()
        }));
    }
    let thinking = find_long_description(&payloads);
    let image = fetch_image(
        page,
        &format!("{}=d-I", image_urls[0]),
        request.wait_timeout_ms,
    )
    .await?;
    let output = AdapterOutput::image(image);
    Ok(if thinking.is_empty() {
        output
    } else {
        output.with_reasoning(thinking)
    })
}

async fn fetch_image(
    page: &dyn PageClient,
    url: &str,
    timeout_ms: u64,
) -> Result<String, AdapterError> {
    let path: PathBuf = std::env::temp_dir().join(format!(
        "webai2api-gemini-{}-{}.bin",
        std::process::id(),
        TEMP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let downloaded = page
        .download_fetch_info(url, &path, json!({}), timeout_ms.max(1))
        .await;
    let (bytes, content_type) = match downloaded {
        Ok(result) => (
            tokio::fs::read(&path)
                .await
                .map_err(|error| AdapterError::new(format!("读取图片下载结果失败: {error}")))?,
            result.content_type,
        ),
        Err(error) => {
            let _ = tokio::fs::remove_file(&path).await;
            return Err(AdapterError::new(format!(
                "图片下载失败: {}",
                error.message
            )));
        }
    };
    let _ = tokio::fs::remove_file(&path).await;
    let mime = content_type
        .as_deref()
        .map(|content_type| content_type.split(';').next().unwrap_or("image/png").trim())
        .filter(|content_type| content_type.starts_with("image/"))
        .map(str::to_owned)
        .or_else(|| {
            Some(
                match image::guess_format(&bytes).ok()? {
                    image::ImageFormat::Jpeg => "image/jpeg",
                    image::ImageFormat::WebP => "image/webp",
                    image::ImageFormat::Gif => "image/gif",
                    _ => "image/png",
                }
                .to_owned(),
            )
        })
        .unwrap_or_else(|| "image/png".to_owned());
    Ok(format!("data:{mime};base64,{}", STANDARD.encode(bytes)))
}

fn parse_len_framed_response(body: &[u8]) -> Result<Vec<Value>, AdapterError> {
    let body = if body.starts_with(b")]}'") {
        body.iter()
            .position(|byte| *byte == b'\n')
            .map(|index| &body[index + 1..])
            .unwrap_or(body)
    } else {
        body
    };
    let text = String::from_utf8_lossy(body);
    let lines: Vec<&str> = text.lines().collect();
    let mut index = 0;
    let mut expect_payload = false;
    let mut frames = Vec::new();
    while index < lines.len() {
        let line = lines[index].trim();
        index += 1;
        if line.is_empty() {
            continue;
        }
        if !expect_payload {
            if line.bytes().all(|byte| byte.is_ascii_digit()) {
                expect_payload = true;
            }
            continue;
        }
        let mut candidate = line.to_owned();
        loop {
            match serde_json::from_str::<Value>(&candidate) {
                Ok(frame) => {
                    frames.push(frame);
                    expect_payload = false;
                    break;
                }
                Err(error) if error.is_eof() && index < lines.len() => {
                    let next = lines[index].trim();
                    if !next.is_empty() && next.bytes().all(|byte| byte.is_ascii_digit()) {
                        return Err(AdapterError::new(format!(
                            "Gemini 响应帧 JSON 截断: {error}"
                        )));
                    }
                    candidate.push('\n');
                    candidate.push_str(next);
                    index += 1;
                }
                Err(error) if error.is_eof() => {
                    return Err(AdapterError::new(format!(
                        "Gemini 响应帧 JSON 截断: {error}"
                    )))
                }
                Err(error) => {
                    return Err(AdapterError::new(format!(
                        "Gemini 响应帧 JSON 解析失败: {error}"
                    )))
                }
            }
        }
    }
    Ok(frames)
}

fn extract_payloads(body: &[u8]) -> Result<Vec<Value>, AdapterError> {
    let mut payloads = Vec::new();
    for frame in parse_len_framed_response(body)? {
        let Some(items) = frame.as_array() else {
            continue;
        };
        for item in items.iter().filter_map(Value::as_array) {
            if let Some(encoded) = item.get(2).and_then(Value::as_str) {
                if let Ok(payload) = serde_json::from_str(encoded) {
                    payloads.push(payload);
                }
            }
        }
    }
    Ok(payloads)
}

fn visit_values<'a>(roots: &'a [Value], mut visit: impl FnMut(&'a Value)) {
    let mut stack: Vec<&Value> = roots.iter().collect();
    while let Some(value) = stack.pop() {
        visit(value);
        match value {
            Value::Array(values) => stack.extend(values.iter()),
            Value::Object(values) => stack.extend(values.values()),
            _ => {}
        }
    }
}

fn collect_image_urls(payloads: &[Value]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut urls = Vec::new();
    visit_values(payloads, |value| {
        if let Some(text) = value.as_str() {
            if text.contains("googleusercontent.com/gg-dl") && seen.insert(text.to_owned()) {
                urls.push(text.to_owned());
            }
        }
    });
    urls
}

fn collect_best_rc_text(payloads: &[Value]) -> String {
    let mut best_by_rc: std::collections::HashMap<String, String> = Default::default();
    visit_values(payloads, |value| {
        let Some(array) = value.as_array() else {
            return;
        };
        let Some(key) = array
            .first()
            .and_then(Value::as_str)
            .filter(|key| key.starts_with("rc_"))
        else {
            return;
        };
        let Some(parts) = array.get(1).and_then(Value::as_array) else {
            return;
        };
        let text: String = parts.iter().filter_map(Value::as_str).collect();
        if text.len() >= best_by_rc.get(key).map_or(0, String::len) && !text.is_empty() {
            best_by_rc.insert(key.to_owned(), text);
        }
    });
    best_by_rc
        .into_values()
        .max_by_key(String::len)
        .unwrap_or_default()
}

fn find_long_description(payloads: &[Value]) -> String {
    let mut best = String::new();
    visit_values(payloads, |value| {
        let Some(text) = value.as_str() else { return };
        if text.chars().count() > 200
            && !text.starts_with("http")
            && !text.starts_with("data:")
            && !text.contains("googleapis.com")
            && !text.contains("googleusercontent.com")
            && !text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"+/=".contains(&byte))
            && text.len() > best.len()
        {
            best = text.to_owned();
        }
    });
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::client::mock::MockPage;

    fn request(model_id: &str) -> GenerateRequest {
        GenerateRequest {
            prompt: "draw a cat".into(),
            model_id: model_id.into(),
            image_paths: vec![],
            reasoning: false,
            wait_timeout_ms: 1,
            adapter_config: Value::Null,
        }
    }

    #[test]
    fn metadata_matches_legacy_manifest() {
        assert_eq!(GeminiAdapter.id(), "gemini");
        assert_eq!(GeminiAdapter.target_url(), TARGET_URL);
        assert_eq!(
            MODEL_IDS,
            ["gemini-3-pro-image-preview", "veo-3.1-generate-preview"]
        );
    }

    #[tokio::test]
    async fn unsupported_model_does_not_touch_page() {
        let page = MockPage::default();
        let result = GeminiAdapter
            .generate(&page, &request("unknown"))
            .await
            .unwrap();
        assert_eq!(result.error.as_deref(), Some("未找到模型配置: unknown"));
        assert!(page.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn parses_len_framed_xssi_payload_and_finds_image_and_thinking() {
        let description = "A long image description ".repeat(12);
        let payload = json!(["rc_answer", ["answer text"], {"url":"https://lh3.googleusercontent.com/gg-dl/image"}, description]);
        let wrapped = json!([["wrb.fr", null, payload.to_string()]]).to_string();
        let body = format!(")]}}'\n{}\n{}", wrapped.len(), wrapped);
        let payloads = extract_payloads(body.as_bytes()).unwrap();
        assert_eq!(
            collect_image_urls(&payloads),
            ["https://lh3.googleusercontent.com/gg-dl/image"]
        );
        assert_eq!(collect_best_rc_text(&payloads), "answer text");
        assert_eq!(find_long_description(&payloads), description);
    }

    #[test]
    fn malformed_truncated_frame_is_reported() {
        let error = parse_len_framed_response(b"4\n[{").unwrap_err();
        assert!(error.message.contains("截断"));
    }
}
