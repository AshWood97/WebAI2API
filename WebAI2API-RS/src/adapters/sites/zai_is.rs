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

pub const TARGET_URL: &str = "https://zai.is/";
pub const MODEL_IDS: &[&str] = &[
    "gemini-3-pro-image-preview",
    "gemini-3-pro-image-preview-2k",
    "gemini-3-pro-image-preview-4k",
    "gemini-2.5-flash-image",
];
const INPUT: &str = ".tiptap.ProseMirror";
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Default, Clone, Copy)]
pub struct ZaiIsAdapter;

impl SiteAdapter for ZaiIsAdapter {
    fn id(&self) -> &'static str {
        "zai_is"
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
            return Err(AdapterError::new("等待 zAI 页面元素超时"));
        }
        call(page, vec![json!({"op":"waitForTimeout","ms":250})]).await?;
    }
}

async fn wait_event<F>(
    page: &dyn PageClient,
    after: &mut u64,
    timeout_ms: u64,
    matches: F,
) -> Result<BrowserEvent, AdapterError>
where
    F: Fn(&BrowserEvent) -> bool,
{
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(timeout_ms) {
        for event in page.poll_events(*after).await? {
            *after = (*after).max(event.sequence);
            if event.event_type == "response" && matches(&event) {
                return Ok(event);
            }
        }
        call(page, vec![json!({"op":"waitForTimeout","ms":200})]).await?;
    }
    Err(AdapterError::normalized(
        "API_TIMEOUT: 等待 zAI 生成响应超时",
        "TIMEOUT_ERROR",
        true,
    ))
}

fn is_request(event: &BrowserEvent, url_part: &str, method: &str) -> bool {
    event
        .url
        .as_deref()
        .is_some_and(|url| url.contains(url_part))
        && event
            .request_method
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case(method))
}

async fn event_body(page: &dyn PageClient, event: &BrowserEvent) -> Result<Vec<u8>, AdapterError> {
    let response_id = event
        .response_id
        .as_deref()
        .ok_or_else(|| AdapterError::new("zAI 响应缺少 responseId"))?;
    page.response_wait_finished(response_id, 30_000).await?;
    page.response_body(response_id).await
}

pub fn image_url_from_completed(value: &Value) -> Option<&str> {
    let id = value.get("id")?.as_str()?;
    let message = value
        .get("messages")?
        .as_array()?
        .iter()
        .find(|message| message.get("id").and_then(Value::as_str) == Some(id))?;
    extract_image_url(message.get("content")?.as_str()?)
}

pub fn extract_image_url(markdown: &str) -> Option<&str> {
    let start = markdown.find("](https://zai.is/")? + 2;
    let tail = &markdown[start..];
    let end = tail.find(')')?;
    let url = &tail[..end];
    (!url.is_empty()).then_some(url)
}

fn model_details(model_id: &str) -> Option<(&'static str, Option<&'static str>)> {
    match model_id {
        "gemini-3-pro-image-preview" => Some(("Nano Banana Pro", Some("1K"))),
        "gemini-3-pro-image-preview-2k" => Some(("Nano Banana Pro", Some("2K"))),
        "gemini-3-pro-image-preview-4k" => Some(("Nano Banana Pro", Some("4K"))),
        "gemini-2.5-flash-image" => Some(("Nano Banana", None)),
        _ => None,
    }
}

async fn handle_discord_auth(page: &dyn PageClient) -> Result<(), AdapterError> {
    let current_url = call(page, vec![json!({"op":"url"})])
        .await?
        .first()
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    if !current_url.contains("zai.is/auth") {
        return Ok(());
    }
    let first_button = chained(css("button"), "first", None);
    wait_visible(page, first_button.clone(), 30_000).await?;
    call(page, vec![op("locator.click", first_button)]).await?;
    call(
        page,
        vec![json!({"op":"waitForURL","url":"**discord.com/oauth2/authorize**","options":{"timeout":60000}})],
    )
    .await?;

    let authorize = css("div[data-align='stretch'] button:last-child");
    let main = css("main");
    for _ in 0..15 {
        if count_result(
            &call(page, vec![op("locator.count", authorize.clone())]).await?,
            0,
        ) > 0
            && call(page, vec![op("locator.enabled", authorize.clone())])
                .await?
                .first()
                .and_then(Value::as_bool)
                .unwrap_or(false)
        {
            call(
                page,
                vec![
                    json!({"op":"waitForTimeout","ms":400}),
                    op("locator.click", authorize.clone()),
                ],
            )
            .await?;
            break;
        }
        if count_result(
            &call(page, vec![op("locator.count", main.clone())]).await?,
            0,
        ) > 0
        {
            let bounds = call(page, vec![op("locator.boundingBox", main.clone())]).await?;
            if let Some(bounds) = bounds.first().filter(|value| !value.is_null()) {
                let x = bounds.get("x").and_then(Value::as_f64).unwrap_or(0.0)
                    + bounds.get("width").and_then(Value::as_f64).unwrap_or(0.0) / 2.0;
                let y = bounds.get("y").and_then(Value::as_f64).unwrap_or(0.0)
                    + bounds.get("height").and_then(Value::as_f64).unwrap_or(0.0) / 2.0;
                call(
                    page,
                    vec![
                        json!({"op":"mouse.move","x":x,"y":y}),
                        json!({"op":"mouse.wheel","deltaY":200}),
                    ],
                )
                .await?;
            }
        }
        call(page, vec![json!({"op":"waitForTimeout","ms":250})]).await?;
    }
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(60) {
        let url = call(page, vec![json!({"op":"url"})])
            .await?
            .first()
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if url.contains("zai.is") && !url.contains("/auth") && !url.contains("discord.com") {
            return Ok(());
        }
        call(page, vec![json!({"op":"waitForTimeout","ms":250})]).await?;
    }
    Err(AdapterError::normalized(
        "等待 Discord 授权返回 zAI 超时",
        "TIMEOUT_ERROR",
        true,
    ))
}

async fn upload_images(
    page: &dyn PageClient,
    paths: &[PathBuf],
    after: &mut u64,
) -> Result<(), AdapterError> {
    if paths.is_empty() {
        return Ok(());
    }
    let inputs = css("input[type='file']");
    let count = count_result(
        &call(page, vec![op("locator.count", inputs.clone())]).await?,
        0,
    );
    let mut uploaded = false;
    let mut last_error = None;
    for index in 0..count {
        let input = chained(inputs.clone(), "nth", Some(index));
        let connected = call(page, vec![op("locator.connected", input.clone())]).await?;
        if !connected.first().and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }
        match call(
            page,
            vec![json!({"op":"upload","locator":input,"files":paths,"options":{"timeout":30000}})],
        )
        .await
        {
            Ok(_) => {
                uploaded = true;
                break;
            }
            Err(error) => last_error = Some(error),
        }
    }
    if !uploaded {
        return Err(last_error.unwrap_or_else(|| AdapterError::new("未找到可用的图片上传控件")));
    }
    let expected = paths.len();
    let mut success_count = 0;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(60) && success_count < expected {
        for event in page.poll_events(*after).await? {
            *after = (*after).max(event.sequence);
            if event.event_type == "response"
                && event.status == Some(200)
                && event
                    .url
                    .as_deref()
                    .is_some_and(|url| url.contains("v1/files"))
            {
                success_count += 1;
            }
        }
        if success_count < expected {
            call(page, vec![json!({"op":"waitForTimeout","ms":200})]).await?;
        }
    }
    if success_count < expected {
        return Err(AdapterError::normalized(
            format!("等待图片上传完成超时 ({success_count}/{expected})"),
            "TIMEOUT_ERROR",
            true,
        ));
    }
    Ok(())
}

async fn choose_model(
    page: &dyn PageClient,
    display_name: &str,
    image_size: Option<&str>,
) -> Result<(), AdapterError> {
    let trigger = role("button", "Select a model");
    wait_visible(page, trigger.clone(), 5_000).await?;
    call(page, vec![op("locator.click", trigger)]).await?;
    let search = role("textbox", "Search In Models");
    wait_visible(page, search.clone(), 5_000).await?;
    call(
        page,
        vec![
            json!({"op":"locator.fill","locator":search,"value":display_name}),
            json!({"op":"locator.press","locator":role("textbox","Search In Models"),"key":"Enter"}),
            json!({"op":"waitForTimeout","ms":700}),
        ],
    )
    .await?;

    let buttons = css("button");
    let count = count_result(
        &call(page, vec![op("locator.count", buttons.clone())]).await?,
        0,
    );
    for index in 0..count {
        let button = chained(buttons.clone(), "nth", Some(index));
        let text = call(page, vec![op("locator.text", button.clone())]).await?;
        let label = text.first().and_then(Value::as_str).unwrap_or("");
        if label.starts_with("GIF Generation") && !label.contains("OFF") {
            call(page, vec![op("locator.click", button)]).await?;
            break;
        }
    }
    if let Some(target_size) = image_size {
        for _ in 0..4 {
            let mut size_button = None;
            let n = count_result(
                &call(page, vec![op("locator.count", buttons.clone())]).await?,
                0,
            );
            for index in 0..n {
                let button = chained(buttons.clone(), "nth", Some(index));
                let text = call(page, vec![op("locator.text", button.clone())]).await?;
                if text
                    .first()
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .starts_with("Image Size")
                {
                    size_button = Some((
                        button,
                        text.first()
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned(),
                    ));
                    break;
                }
            }
            let Some((button, current)) = size_button else {
                break;
            };
            if current.contains(target_size) {
                break;
            }
            call(
                page,
                vec![
                    op("locator.click", button),
                    json!({"op":"waitForTimeout","ms":350}),
                ],
            )
            .await?;
        }
    }
    Ok(())
}

async fn fetch_image(
    page: &dyn PageClient,
    url: &str,
    timeout_ms: u64,
) -> Result<String, AdapterError> {
    let path = std::env::temp_dir().join(format!(
        "webai2api-zai-{}-{}.img",
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
                "下载 zAI 图片失败: {}",
                error.message
            )));
        }
    };
    let bytes = tokio::fs::read(&path).await;
    let _ = tokio::fs::remove_file(&path).await;
    let bytes = bytes.map_err(|error| AdapterError::new(format!("读取 zAI 图片失败: {error}")))?;
    let mime = fetched
        .content_type
        .and_then(|value| value.split(';').next().map(str::trim).map(str::to_owned))
        .filter(|value| value.starts_with("image/"))
        .unwrap_or_else(|| "image/png".to_owned());
    Ok(format!("data:{mime};base64,{}", STANDARD.encode(bytes)))
}

async fn generate(
    page: &dyn PageClient,
    request: &GenerateRequest,
) -> Result<AdapterOutput, AdapterError> {
    call(
        page,
        vec![json!({"op":"goto","url":TARGET_URL,"options":{"waitUntil":"load","timeout":120000}})],
    )
    .await
    .map_err(|error| page_error(&error.message).unwrap_or(error))?;
    handle_discord_auth(page).await?;
    let input = css(INPUT);
    wait_visible(page, input.clone(), 90_000).await?;
    let mut after = page.subscribe_responses().await?;
    upload_images(page, &request.image_paths, &mut after).await?;

    call(
        page,
        vec![
            op("locator.click", input.clone()),
            json!({"op":"locator.fill","locator":input,"value":request.prompt}),
        ],
    )
    .await?;
    let (display_name, image_size) = model_details(&request.model_id)
        .ok_or_else(|| AdapterError::new(format!("未找到模型配置: {}", request.model_id)))?;
    choose_model(page, display_name, image_size).await?;

    let send = css("button[type='submit']");
    wait_visible(page, send.clone(), 10_000).await?;
    call(page, vec![op("locator.click", send)]).await?;

    let chats_event = wait_event(page, &mut after, 60_000, |event| {
        is_request(event, "v1/chats/new", "POST")
    })
    .await?;
    let chats_body = event_body(page, &chats_event).await?;
    let chats_text = String::from_utf8_lossy(&chats_body);
    if let Some(error) = http_error(chats_event.status.unwrap_or_default(), &chats_text) {
        return Err(AdapterError::new(format!(
            "创建对话失败: {}",
            error.message
        )));
    }
    let chats_json: Value = serde_json::from_slice(&chats_body)
        .map_err(|error| AdapterError::new(format!("解析对话响应失败: {error}")))?;
    if chats_json.get("id").and_then(Value::as_str).is_none() {
        return Err(AdapterError::new("创建对话响应中没有 id"));
    }

    let completions_event = wait_event(page, &mut after, request.wait_timeout_ms, |event| {
        is_request(event, "chat/completions", "POST")
    })
    .await?;
    let completions_body = event_body(page, &completions_event).await?;
    let completions_text = String::from_utf8_lossy(&completions_body);
    if let Some(error) = http_error(
        completions_event.status.unwrap_or_default(),
        &completions_text,
    ) {
        return Err(AdapterError::new(format!(
            "生成请求失败: {}",
            error.message
        )));
    }
    let completions_json: Value = serde_json::from_slice(&completions_body)
        .map_err(|error| AdapterError::new(format!("解析生成响应失败: {error}")))?;
    if !completions_json
        .get("status")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(AdapterError::new("生成失败，响应状态异常"));
    }

    let completed_event = wait_event(page, &mut after, request.wait_timeout_ms, |event| {
        is_request(event, "chat/completed", "POST")
    })
    .await?;
    let completed_body = event_body(page, &completed_event).await?;
    let completed_text = String::from_utf8_lossy(&completed_body);
    if let Some(error) = http_error(completed_event.status.unwrap_or_default(), &completed_text) {
        return Err(AdapterError::new(format!(
            "生成请求失败: {}",
            error.message
        )));
    }
    let completed_json: Value = serde_json::from_slice(&completed_body)
        .map_err(|error| AdapterError::new(format!("解析完成响应失败: {error}")))?;
    let image_url = image_url_from_completed(&completed_json)
        .ok_or_else(|| AdapterError::new("回复中未找到图片链接"))?;
    Ok(AdapterOutput::image(
        fetch_image(page, image_url, request.wait_timeout_ms.max(1)).await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::client::mock::MockPage;

    #[test]
    fn metadata_and_model_options_match_legacy_manifest() {
        assert_eq!(ZaiIsAdapter.id(), "zai_is");
        assert_eq!(ZaiIsAdapter.target_url(), TARGET_URL);
        assert_eq!(ZaiIsAdapter.model_ids(), MODEL_IDS);
        assert_eq!(
            model_details("gemini-3-pro-image-preview"),
            Some(("Nano Banana Pro", Some("1K")))
        );
        assert_eq!(
            model_details("gemini-3-pro-image-preview-2k"),
            Some(("Nano Banana Pro", Some("2K")))
        );
        assert_eq!(
            model_details("gemini-3-pro-image-preview-4k"),
            Some(("Nano Banana Pro", Some("4K")))
        );
        assert_eq!(
            model_details("gemini-2.5-flash-image"),
            Some(("Nano Banana", None))
        );
    }

    #[test]
    fn extracts_image_markdown_url_from_matching_completed_message() {
        let body = json!({
            "id":"answer-2",
            "messages":[
                {"id":"answer-1","content":"![old](https://zai.is/old.png)"},
                {"id":"answer-2","content":"Done\n![generated](https://zai.is/files/abc.png?x=1)"}
            ]
        });
        assert_eq!(
            image_url_from_completed(&body),
            Some("https://zai.is/files/abc.png?x=1")
        );
        assert_eq!(
            image_url_from_completed(&json!({"id":"x","messages":[]})),
            None
        );
        assert_eq!(extract_image_url("No image here"), None);
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
        let result = ZaiIsAdapter.generate(&page, &request).await.unwrap();
        assert_eq!(result.error.as_deref(), Some("未找到模型配置: missing"));
        assert!(page.calls.lock().unwrap().is_empty());
    }
}
