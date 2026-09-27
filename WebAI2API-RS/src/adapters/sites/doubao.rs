use super::super::client::{
    chained, count_result, css, op, page_error, AdapterError, AdapterOutput, BrowserEvent,
    ClientFuture, GenerateRequest, PageClient, SiteAdapter,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

pub const TARGET_URL: &str = "https://www.doubao.com/chat/";
pub const MODELS: &[(&str, &str)] = &[
    ("seedream-4.5", "Seedream 4.5"),
    ("seedream-4.0", "Seedream 4.0"),
    ("seedream-5.0-lite", "Seedream 5.0 Lite"),
];
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Default, Clone, Copy)]
pub struct DoubaoAdapter;

impl SiteAdapter for DoubaoAdapter {
    fn id(&self) -> &'static str {
        "doubao"
    }
    fn target_url(&self) -> &'static str {
        TARGET_URL
    }
    fn model_ids(&self) -> &'static [&'static str] {
        &["seedream-4.5", "seedream-4.0", "seedream-5.0-lite"]
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
            return Err(AdapterError::new("等待豆包页面元素超时"));
        }
        call(page, vec![json!({"op":"waitForTimeout","ms":250})]).await?;
    }
}

async fn wait_event<F>(
    page: &dyn PageClient,
    mut after: u64,
    timeout_ms: u64,
    matches: F,
) -> Result<BrowserEvent, AdapterError>
where
    F: Fn(&BrowserEvent) -> bool,
{
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(timeout_ms) {
        for event in page.poll_events(after).await? {
            after = after.max(event.sequence);
            if event.event_type == "response" && matches(&event) {
                return Ok(event);
            }
        }
        call(page, vec![json!({"op":"waitForTimeout","ms":200})]).await?;
    }
    Err(AdapterError::normalized(
        "API_TIMEOUT: 等待豆包图片生成响应超时",
        "TIMEOUT_ERROR",
        true,
    ))
}

async fn response_body(
    page: &dyn PageClient,
    event: &BrowserEvent,
) -> Result<String, AdapterError> {
    let bytes = page
        .response_body(
            event
                .response_id
                .as_deref()
                .ok_or_else(|| AdapterError::new("豆包响应缺少 responseId"))?,
        )
        .await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn is_sse_response(event: &BrowserEvent) -> bool {
    event
        .headers
        .as_ref()
        .and_then(|headers| headers.get("content-type"))
        .and_then(Value::as_str)
        .is_some_and(|content_type| content_type.contains("text/event-stream"))
}

fn extract_store_uri(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    value
        .pointer("/Result/UploadAddress/StoreInfos/0/StoreUri")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

async fn upload_references(
    page: &dyn PageClient,
    files: &[PathBuf],
    timeout_ms: u64,
) -> Result<(), AdapterError> {
    let reference_button = css("button");
    let count = count_result(
        &call(page, vec![op("locator.count", reference_button.clone())]).await?,
        0,
    );
    // Match the localized upload control in Rust from its rendered text.
    let mut trigger = None;
    for index in 0..count {
        let button = chained(reference_button.clone(), "nth", Some(index));
        let text = call(page, vec![op("locator.text", button.clone())])
            .await?
            .first()
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if ["Reference Image", "参考图", "參考圖"]
            .iter()
            .any(|needle| text.contains(needle))
        {
            trigger = Some(button);
            break;
        }
    }
    let trigger = trigger.ok_or_else(|| AdapterError::new("未找到豆包参考图上传按钮"))?;
    let mut after = page.subscribe_responses().await?;
    call(page, vec![json!({"op":"filechooser.clickAndSetFiles","locator":trigger,"files":files,"options":{"timeout":30000}})]).await?;
    let started = Instant::now();
    let mut store_paths: Vec<String> = Vec::new();
    let mut uploaded = vec![false; files.len()];
    while started.elapsed() < Duration::from_millis(timeout_ms) {
        for event in page.poll_events(after).await? {
            after = after.max(event.sequence);
            if event.event_type != "response" || event.status != Some(200) {
                continue;
            }
            let Some(url) = event.url.as_deref() else {
                continue;
            };
            if url.contains("Action=ApplyImageUpload") {
                if let Some(path) = extract_store_uri(&response_body(page, &event).await?) {
                    store_paths.push(path);
                }
                continue;
            }
            if event
                .request_method
                .as_deref()
                .is_some_and(|method| method.eq_ignore_ascii_case("POST"))
            {
                for (index, path) in store_paths.iter().enumerate() {
                    if !uploaded[index] && url.contains(path) {
                        uploaded[index] = true;
                    }
                }
            }
        }
        if uploaded.iter().all(|done| *done) {
            return Ok(());
        }
        call(page, vec![json!({"op":"waitForTimeout","ms":200})]).await?;
    }
    Err(AdapterError::normalized(
        "等待豆包图片上传完成超时",
        "TIMEOUT_ERROR",
        true,
    ))
}

fn extract_raw_image(value: &Value) -> Option<String> {
    if let Some(operations) = value.get("patch_op").and_then(Value::as_array) {
        for operation in operations {
            if let Some(blocks) = operation
                .pointer("/patch_value/content_block")
                .and_then(Value::as_array)
            {
                for block in blocks {
                    if block.get("block_type").and_then(Value::as_u64) == Some(2074) {
                        if let Some(content) = block.pointer("/content/creation_block") {
                            if let Some(url) = extract_raw_image(content) {
                                return Some(url);
                            }
                        }
                    }
                }
            }
        }
    }
    value
        .get("creations")
        .and_then(Value::as_array)?
        .iter()
        .find_map(|creation| {
            creation
                .pointer("/image/image_ori_raw/url")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
}

pub fn parse_sse_for_image(body: &str) -> Option<String> {
    for line in body
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("data:"))
    {
        let data = line[5..].trim();
        if data.is_empty() || data == "{}" {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        if let Some(event_data) = value.get("event_data") {
            let parsed = match event_data {
                Value::String(s) => serde_json::from_str::<Value>(s).ok(),
                other => Some(other.clone()),
            };
            let Some(message) = parsed.as_ref().and_then(|parsed| parsed.get("message")) else {
                continue;
            };
            if message.get("content_type").and_then(Value::as_u64) != Some(2074) {
                continue;
            }
            let Some(raw_content) = message.get("content") else {
                continue;
            };
            let content = match raw_content {
                Value::String(s) => match serde_json::from_str::<Value>(s) {
                    Ok(value) => value,
                    Err(_) => continue,
                },
                other => other.clone(),
            };
            if let Some(url) = extract_raw_image(&content) {
                return Some(url);
            }
            continue;
        }
        if let Some(url) = extract_raw_image(&value) {
            return Some(url);
        }
    }
    None
}

async fn fetch_image(
    page: &dyn PageClient,
    url: &str,
    timeout_ms: u64,
) -> Result<String, AdapterError> {
    let path = std::env::temp_dir().join(format!(
        "webai2api-doubao-{}-{}.bin",
        std::process::id(),
        TEMP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let fetched = match page
        .download_fetch_info(url, &path, json!({}), timeout_ms)
        .await
    {
        Ok(result) => result,
        Err(error) => {
            let _ = tokio::fs::remove_file(&path).await;
            return Err(AdapterError::new(format!(
                "已获取结果，但图片下载时遇到错误: {}",
                error.message
            )));
        }
    };
    let bytes = tokio::fs::read(&path).await;
    let _ = tokio::fs::remove_file(&path).await;
    let bytes = bytes.map_err(|error| AdapterError::new(format!("读取下载图片失败: {error}")))?;
    let mime = fetched
        .content_type
        .map(|content_type| {
            content_type
                .split(';')
                .next()
                .unwrap_or("image/png")
                .trim()
                .to_owned()
        })
        .filter(|content_type| content_type.starts_with("image/"))
        .unwrap_or_else(|| match image::guess_format(&bytes).ok() {
            Some(image::ImageFormat::Jpeg) => "image/jpeg".to_owned(),
            Some(image::ImageFormat::WebP) => "image/webp".to_owned(),
            Some(image::ImageFormat::Gif) => "image/gif".to_owned(),
            _ => "image/png".to_owned(),
        });
    Ok(format!("data:{mime};base64,{}", STANDARD.encode(bytes)))
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
    let model_name = MODELS
        .iter()
        .find(|(id, _)| *id == request.model_id)
        .map(|(_, name)| *name)
        .unwrap_or(MODELS[0].1);
    let skill = css("button[data-skill-id='skill_bar_button_3']");
    wait_visible(page, skill.clone(), 30_000).await?;
    call(page, vec![op("locator.click", skill)]).await?;
    let buttons = css("button");
    let count = count_result(
        &call(page, vec![op("locator.count", buttons.clone())]).await?,
        0,
    );
    let mut model_trigger = None;
    for index in 0..count {
        let button = chained(buttons.clone(), "nth", Some(index));
        let text = call(page, vec![op("locator.text", button.clone())])
            .await?
            .first()
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if text.to_ascii_lowercase().contains("seedream") {
            model_trigger = Some(button);
            break;
        }
    }
    let model_trigger = model_trigger.ok_or_else(|| AdapterError::new("未找到豆包模型选择器"))?;
    wait_visible(page, model_trigger.clone(), 10_000).await?;
    call(
        page,
        vec![
            op("locator.click", model_trigger),
            json!({"op":"waitForTimeout","ms":400}),
        ],
    )
    .await?;
    let menu_items = css("[role='menuitem']");
    let count = count_result(
        &call(page, vec![op("locator.count", menu_items.clone())]).await?,
        0,
    );
    let mut selected = false;
    for index in 0..count {
        let item = chained(menu_items.clone(), "nth", Some(index));
        let text = call(page, vec![op("locator.text", item.clone())])
            .await?
            .first()
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if text.contains(model_name) {
            call(page, vec![op("locator.click", item)]).await?;
            selected = true;
            break;
        }
    }
    if !selected {
        return Err(AdapterError::new(format!("未找到模型选项: {model_name}")));
    }

    if !request.image_paths.is_empty() {
        upload_references(page, &request.image_paths, 60_000).await?;
    }
    let input = css("#input-engine-container div[role='textbox']");
    wait_visible(page, input.clone(), 30_000).await?;
    call(
        page,
        vec![
            op("locator.click", input.clone()),
            json!({"op":"locator.fill","locator":input,"value":request.prompt}),
        ],
    )
    .await?;
    let cursor = page.subscribe_responses().await?;
    let send = css("button#flow-end-msg-send");
    wait_visible(page, send.clone(), 10_000).await?;
    call(page, vec![op("locator.click", send)]).await?;
    let event = wait_event(page, cursor, request.wait_timeout_ms.max(1), |event| {
        event
            .request_method
            .as_deref()
            .is_some_and(|method| method.eq_ignore_ascii_case("POST"))
            && event
                .url
                .as_deref()
                .is_some_and(|url| url.contains("chat/completion"))
            && is_sse_response(event)
    })
    .await?;
    let body = response_body(page, &event).await?;
    let image_url =
        parse_sse_for_image(&body).ok_or_else(|| AdapterError::new("未能从响应中提取图片链接"))?;
    Ok(AdapterOutput::image(
        fetch_image(page, &image_url, request.wait_timeout_ms.max(1)).await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::client::mock::MockPage;

    #[test]
    fn metadata_matches_legacy_manifest() {
        assert_eq!(DoubaoAdapter.id(), "doubao");
        assert_eq!(DoubaoAdapter.target_url(), TARGET_URL);
        assert_eq!(
            MODELS,
            &[
                ("seedream-4.5", "Seedream 4.5"),
                ("seedream-4.0", "Seedream 4.0"),
                ("seedream-5.0-lite", "Seedream 5.0 Lite")
            ]
        );
    }

    #[test]
    fn parses_both_doubao_sse_shapes() {
        let old = "data: {\"creations\":[{\"image\":{\"image_ori_raw\":{\"url\":\"https://img/old.png\"}}}]}\n";
        assert_eq!(
            parse_sse_for_image(old).as_deref(),
            Some("https://img/old.png")
        );
        let content =
            r#"{"creations":[{"image":{"image_ori_raw":{"url":"https://img/new.png"}}}]}"#;
        let event_data = json!({"message":{"content_type":2074,"content":content}}).to_string();
        let sse = format!("data: {}\n", json!({"event_data":event_data}));
        assert_eq!(
            parse_sse_for_image(&sse).as_deref(),
            Some("https://img/new.png")
        );
        let patch = json!({"patch_op":[{"patch_value":{"content_block":[{"block_type":2074,"content":{"creation_block":{"creations":[{"image":{"image_ori_raw":{"url":"https://img/patch.png"}}}]}}}]}}]});
        assert_eq!(
            extract_raw_image(&patch).as_deref(),
            Some("https://img/patch.png")
        );
    }

    #[test]
    fn response_filter_requires_event_stream_content_type() {
        let mut event = BrowserEvent {
            sequence: 1,
            event_type: "response".into(),
            browser_id: None,
            page_id: None,
            generation: None,
            response_id: Some("id".into()),
            url: Some("https://x/chat/completion".into()),
            status: Some(200),
            request_method: Some("POST".into()),
            headers: Some(json!({"content-type":"application/json"})),
            route_token: None,
            request: None,
        };
        assert!(!is_sse_response(&event));
        event.headers = Some(json!({"content-type":"text/event-stream; charset=utf-8"}));
        assert!(is_sse_response(&event));
    }

    #[tokio::test]
    async fn rejects_unknown_model_without_page_calls() {
        let page = MockPage::default();
        let request = GenerateRequest {
            prompt: "x".into(),
            model_id: "missing".into(),
            image_paths: vec![],
            reasoning: false,
            wait_timeout_ms: 1,
            adapter_config: Value::Null,
        };
        let result = DoubaoAdapter.generate(&page, &request).await.unwrap();
        assert_eq!(result.error.as_deref(), Some("未找到模型配置: missing"));
        assert!(page.calls.lock().unwrap().is_empty());
    }
}
