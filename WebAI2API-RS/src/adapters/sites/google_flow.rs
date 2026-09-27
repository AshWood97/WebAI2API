use super::super::client::{
    chained, count_result, css, http_error, op, page_error, role, AdapterError, AdapterOutput,
    BrowserEvent, ClientFuture, GenerateRequest, PageClient, SiteAdapter,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

pub const TARGET_URL: &str = "https://labs.google/fx/zh/tools/flow";
pub const MODELS: &[(&str, &str, &str)] = &[
    ("gemini-3-pro-image-preview", "🍌 Nano Banana Pro", "0"),
    ("gemini-2.5-flash-image-preview", "🍌 Nano Banana", "0"),
    ("imagen-4", "Imagen 4", "0"),
    (
        "gemini-3-pro-image-preview-landspace",
        "🍌 Nano Banana Pro",
        "16:9",
    ),
    (
        "gemini-3-pro-image-preview-portrait",
        "🍌 Nano Banana Pro",
        "9:16",
    ),
    (
        "gemini-2.5-flash-image-preview-landspace",
        "🍌 Nano Banana",
        "16:9",
    ),
    (
        "gemini-2.5-flash-image-preview-portrait",
        "🍌 Nano Banana",
        "9:16",
    ),
    ("imagen-4-landspace", "Imagen 4", "16:9"),
    ("imagen-4-portrait", "Imagen 4", "9:16"),
];
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

fn image_aspect(width: u32, height: u32) -> &'static str {
    if width >= height {
        "16:9"
    } else {
        "9:16"
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct GoogleFlowAdapter;

impl SiteAdapter for GoogleFlowAdapter {
    fn id(&self) -> &'static str {
        "google_flow"
    }
    fn target_url(&self) -> &'static str {
        TARGET_URL
    }
    fn model_ids(&self) -> &'static [&'static str] {
        &[
            "gemini-3-pro-image-preview",
            "gemini-2.5-flash-image-preview",
            "imagen-4",
            "gemini-3-pro-image-preview-landspace",
            "gemini-3-pro-image-preview-portrait",
            "gemini-2.5-flash-image-preview-landspace",
            "gemini-2.5-flash-image-preview-portrait",
            "imagen-4-landspace",
            "imagen-4-portrait",
        ]
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
            return Err(AdapterError::new("等待 Google Flow 页面元素超时"));
        }
        call(page, vec![json!({"op":"waitForTimeout","ms":250})]).await?;
    }
}

async fn wait_response(
    page: &dyn PageClient,
    cursor: u64,
    needle: &str,
    timeout_ms: u64,
) -> Result<BrowserEvent, AdapterError> {
    let started = Instant::now();
    let mut after = cursor;
    while started.elapsed() < Duration::from_millis(timeout_ms) {
        for event in page.poll_events(after).await? {
            after = after.max(event.sequence);
            if event.event_type == "response"
                && event.url.as_deref().is_some_and(|url| url.contains(needle))
            {
                return Ok(event);
            }
        }
        call(page, vec![json!({"op":"waitForTimeout","ms":200})]).await?;
    }
    Err(AdapterError::normalized(
        format!("API_TIMEOUT: 等待 {needle} 响应超时"),
        "TIMEOUT_ERROR",
        true,
    ))
}

async fn click_text_option(page: &dyn PageClient, option_text: &str) -> Result<bool, AdapterError> {
    let options = css("[role='option']");
    let count = count_result(
        &call(page, vec![op("locator.count", options.clone())]).await?,
        0,
    );
    for index in 0..count {
        let item = chained(options.clone(), "nth", Some(index));
        let text = call(page, vec![op("locator.text", item.clone())])
            .await?
            .first()
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if text.trim() == option_text || text.contains(option_text) {
            call(page, vec![op("locator.click", item)]).await?;
            return Ok(true);
        }
    }
    Ok(false)
}

async fn select_combo_by_text(
    page: &dyn PageClient,
    needles: &[&str],
    option: &str,
    exclusions: &[&str],
) -> Result<bool, AdapterError> {
    let combos = css("select, [role='combobox']");
    let count = count_result(
        &call(page, vec![op("locator.count", combos.clone())]).await?,
        0,
    );
    for index in 0..count {
        let combo = chained(combos.clone(), "nth", Some(index));
        let text = call(page, vec![op("locator.text", combo.clone())])
            .await?
            .first()
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if !needles.iter().any(|needle| text.contains(needle))
            || exclusions.iter().any(|needle| text.contains(needle))
        {
            continue;
        }
        call(page, vec![op("locator.click", combo)]).await?;
        call(page, vec![json!({"op":"waitForTimeout","ms":300})]).await?;
        return click_text_option(page, option).await;
    }
    Ok(false)
}

async fn upload_image(
    page: &dyn PageClient,
    file: &PathBuf,
    timeout_ms: u64,
) -> Result<(), AdapterError> {
    let add = role("button", "add");
    wait_visible(page, add.clone(), 30_000).await?;
    let cursor = page.subscribe_responses().await?;
    call(page, vec![op("locator.click", add)]).await?;
    call(
        page,
        vec![
            json!({"op":"locator.remove","locator":css("div[class*='virtuoso-grid-list'] > :not(:first-child)")}),
            json!({"op":"locator.setStyle","locator":css("[data-radix-popper-content-wrapper]"),"styles":{"height":"335px","transform":"translate(0px, -391px)"}}),
        ],
    )
    .await?;
    let upload_button = role("button", "upload");
    call(page, vec![json!({"op":"filechooser.clickAndSetFiles","locator":upload_button,"files":[file],"options":{"timeout":30000}})]).await?;
    let crop = role("button", "crop");
    wait_visible(page, crop.clone(), 30_000).await?;
    call(page, vec![op("locator.click", crop)]).await?;
    wait_response(page, cursor, "v1:uploadUserImage", timeout_ms).await?;
    Ok(())
}

async fn fetch_image(
    page: &dyn PageClient,
    url: &str,
    timeout_ms: u64,
) -> Result<String, AdapterError> {
    let path = std::env::temp_dir().join(format!(
        "webai2api-flow-{}-{}.bin",
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
        .or_else(|| {
            Some(
                match image::guess_format(&bytes).ok()? {
                    image::ImageFormat::Jpeg => "image/jpeg",
                    image::ImageFormat::WebP => "image/webp",
                    image::ImageFormat::Gif => "image/gif",
                    image::ImageFormat::Png => "image/png",
                    _ => "image/png",
                }
                .to_owned(),
            )
        })
        .unwrap_or_else(|| "image/png".to_owned());
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
    let (model_name, model_size) = MODELS
        .iter()
        .find(|(id, _, _)| *id == request.model_id)
        .map(|(_, name, size)| (*name, *size))
        .unwrap_or((MODELS[0].1, MODELS[0].2));
    let aspect = if model_size == "0" {
        if let Some(path) = request.image_paths.first() {
            tokio::task::spawn_blocking({
                let path = path.clone();
                move || {
                    image::image_dimensions(path)
                        .map(|(width, height)| image_aspect(width, height))
                        .unwrap_or("16:9")
                }
            })
            .await
            .unwrap_or("16:9")
        } else {
            "16:9"
        }
    } else {
        model_size
    };

    let add_project = role("button", "add_2");
    wait_visible(page, add_project.clone(), 30_000).await?;
    call(page, vec![op("locator.click", add_project)]).await?;
    // The generic runtime locator contract does not support nested `has`
    // locators. Select the image mode from the visible option text instead.
    let mode = css("select, [role='combobox']");
    wait_visible(page, mode.clone(), 30_000).await?;
    call(
        page,
        vec![op("locator.click", chained(mode.clone(), "first", None))],
    )
    .await?;
    if !click_text_option(page, "add_photo_alternate").await? {
        let _ = click_text_option(page, "Images").await?;
    }

    let tune = role("button", "tune");
    wait_visible(page, tune.clone(), 30_000).await?;
    call(
        page,
        vec![
            op("locator.click", tune),
            json!({"op":"waitForTimeout","ms":400}),
        ],
    )
    .await?;
    let _ = select_combo_by_text(
        page,
        &["1", "2", "3", "4"],
        "1",
        &["Banana", "Imagen", "16:9", "9:16", "1:1", "4:3", "3:4"],
    )
    .await?;
    let _ = select_combo_by_text(page, &["Nano Banana", "Imagen 4"], model_name, &[]).await?;
    let _ = select_combo_by_text(page, &["16:9", "9:16"], aspect, &[]).await?;

    for file in &request.image_paths {
        upload_image(page, file, 60_000).await?;
    }
    let input = css("textarea[placeholder]");
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
    let send = role("button", "arrow_forward");
    wait_visible(page, send.clone(), 30_000).await?;
    call(page, vec![op("locator.click", send)]).await?;
    let event = wait_response(
        page,
        cursor,
        "flowMedia:batchGenerateImages",
        request.wait_timeout_ms.max(1),
    )
    .await?;
    let id = event
        .response_id
        .as_deref()
        .ok_or_else(|| AdapterError::new("Google Flow 响应缺少 responseId"))?;
    let body_bytes = page.response_body(id).await?;
    let body = String::from_utf8_lossy(&body_bytes);
    if let Some(error) = http_error(event.status.unwrap_or(200), &body) {
        return Err(error);
    }
    let response: Value = serde_json::from_str(&body)
        .map_err(|error| AdapterError::new(format!("解析响应失败: {error}")))?;
    let image_url = response
        .pointer("/media/0/image/generatedImage/fifeUrl")
        .and_then(Value::as_str)
        .ok_or_else(|| AdapterError::new("生成成功但响应中没有图片 URL"))?;
    Ok(AdapterOutput::image(
        fetch_image(page, image_url, request.wait_timeout_ms.max(1)).await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::client::mock::MockPage;

    #[test]
    fn metadata_matches_legacy_manifest() {
        assert_eq!(GoogleFlowAdapter.id(), "google_flow");
        assert_eq!(GoogleFlowAdapter.target_url(), TARGET_URL);
        assert_eq!(GoogleFlowAdapter.model_ids().len(), 9);
        assert_eq!(
            MODELS[0],
            ("gemini-3-pro-image-preview", "🍌 Nano Banana Pro", "0")
        );
    }

    #[test]
    fn image_aspect_matches_legacy_orientation_rule() {
        assert_eq!(image_aspect(1600, 900), "16:9");
        assert_eq!(image_aspect(900, 1600), "9:16");
        assert_eq!(image_aspect(1, 1), "16:9");
    }

    #[tokio::test]
    async fn rejects_unlisted_model_without_page_calls() {
        let page = MockPage::default();
        let request = GenerateRequest {
            prompt: "x".into(),
            model_id: "missing".into(),
            image_paths: vec![],
            reasoning: false,
            wait_timeout_ms: 1,
            adapter_config: Value::Null,
        };
        let result = GoogleFlowAdapter.generate(&page, &request).await.unwrap();
        assert_eq!(result.error.as_deref(), Some("未找到模型配置: missing"));
        assert!(page.calls.lock().unwrap().is_empty());
    }
}
