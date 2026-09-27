use super::super::client::{
    count_result, css, op, page_error, role, AdapterError, AdapterOutput, BrowserEvent,
    ClientFuture, GenerateRequest, PageClient, SiteAdapter,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

pub const TARGET_URL: &str = "https://sora.chatgpt.com/profile";
pub const MODEL_IDS: &[&str] = &["sora-2"];
const INPUT: &str = "textarea";
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Default, Clone, Copy)]
pub struct SoraAdapter;

impl SiteAdapter for SoraAdapter {
    fn id(&self) -> &'static str {
        "sora"
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
            return Err(AdapterError::new("等待 Sora 页面输入控件超时"));
        }
        call(page, vec![json!({"op":"waitForTimeout","ms":250})]).await?;
    }
}

async fn wait_response<F>(
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
        "API_TIMEOUT: 等待 Sora 视频生成响应超时",
        "TIMEOUT_ERROR",
        true,
    ))
}

async fn event_body(page: &dyn PageClient, event: &BrowserEvent) -> Result<Value, AdapterError> {
    let response_id = event
        .response_id
        .as_deref()
        .ok_or_else(|| AdapterError::new("Sora 响应缺少 responseId"))?;
    page.response_wait_finished(response_id, 30_000).await?;
    let bytes = page.response_body(response_id).await?;
    serde_json::from_slice(&bytes)
        .map_err(|error| AdapterError::new(format!("解析 Sora 响应失败: {error}")))
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

pub fn task_id_from_create(value: &Value) -> Option<&str> {
    value.get("id").and_then(Value::as_str)
}

pub fn task_is_pending(value: &Value, task_id: &str) -> bool {
    value.as_array().is_some_and(|items| {
        items
            .iter()
            .any(|item| item.get("id").and_then(Value::as_str) == Some(task_id))
    })
}

pub fn video_url_from_drafts<'a>(value: &'a Value, task_id: &str) -> Option<&'a str> {
    let items = value
        .get("items")
        .and_then(Value::as_array)
        .or_else(|| value.as_array())?;
    items
        .iter()
        .find(|item| item.get("task_id").and_then(Value::as_str) == Some(task_id))?
        .get("url")
        .and_then(Value::as_str)
}

async fn upload_first_reference(
    page: &dyn PageClient,
    path: &PathBuf,
    after: &mut u64,
) -> Result<(), AdapterError> {
    let button = role("button", "Attach media");
    let upload_event = wait_response(page, after, 60_000, |event| {
        event.status == Some(200)
            && event
                .url
                .as_deref()
                .is_some_and(|url| url.contains("project_y/file/upload"))
    });
    call(
        page,
        vec![json!({
            "op":"filechooser.clickAndSetFiles",
            "locator":button,
            "files":[path],
            "options":{"timeout":60000}
        })],
    )
    .await?;
    upload_event.await.map(|_| ())
}

async fn download_video(
    page: &dyn PageClient,
    url: &str,
    timeout_ms: u64,
) -> Result<String, AdapterError> {
    let path = std::env::temp_dir().join(format!(
        "webai2api-sora-{}-{}.mp4",
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
                "下载 Sora 视频失败: {}",
                error.message
            )));
        }
    };
    let bytes = tokio::fs::read(&path).await;
    let _ = tokio::fs::remove_file(&path).await;
    let bytes = bytes.map_err(|error| AdapterError::new(format!("读取 Sora 视频失败: {error}")))?;
    let mime = fetched
        .content_type
        .and_then(|value| value.split(';').next().map(str::trim).map(str::to_owned))
        .filter(|value| value.starts_with("video/"))
        .unwrap_or_else(|| "video/mp4".to_owned());
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

    let input = css(INPUT);
    wait_visible(page, input.clone(), 90_000).await?;
    let mut after = page.subscribe_responses().await?;

    if let Some(image) = request.image_paths.first() {
        upload_first_reference(page, image, &mut after).await?;
    }
    call(
        page,
        vec![
            op("locator.click", input.clone()),
            json!({"op":"locator.fill","locator":input,"value":request.prompt}),
        ],
    )
    .await?;

    let create_button = role("button", "Create video");
    let create_event = wait_response(page, &mut after, 60_000, |event| {
        event.status == Some(200) && is_request(event, "nf/create", "POST")
    });
    call(page, vec![op("locator.click", create_button)]).await?;
    let create_response = create_event.await?;
    let create_body = event_body(page, &create_response).await?;
    let task_id = task_id_from_create(&create_body)
        .ok_or_else(|| AdapterError::new("创建任务失败：响应中没有 id"))?
        .to_owned();

    let deadline = Instant::now() + Duration::from_secs(300);
    let mut drafts_body = None;
    let mut task_completed = false;
    while Instant::now() < deadline && !task_completed {
        for event in page.poll_events(after).await? {
            after = after.max(event.sequence);
            if event.event_type != "response" || event.status != Some(200) {
                continue;
            }
            if is_request(&event, "project_y/profile/drafts", "GET") {
                drafts_body = Some(event_body(page, &event).await?);
            } else if is_request(&event, "nf/pending/v2", "GET") {
                let body = event_body(page, &event).await?;
                if !task_is_pending(&body, &task_id) {
                    task_completed = true;
                    break;
                }
            }
        }
        if !task_completed {
            call(page, vec![json!({"op":"waitForTimeout","ms":200})]).await?;
        }
    }
    if !task_completed {
        return Err(AdapterError::normalized(
            "等待视频生成超时 (5分钟)",
            "TIMEOUT_ERROR",
            true,
        ));
    }
    let drafts = if let Some(body) = drafts_body {
        body
    } else {
        let response = wait_response(page, &mut after, 60_000, |event| {
            event.status == Some(200) && is_request(event, "project_y/profile/drafts", "GET")
        })
        .await?;
        event_body(page, &response).await?
    };
    let video_url = video_url_from_drafts(&drafts, &task_id)
        .ok_or_else(|| AdapterError::new("未找到匹配的视频任务或视频 URL"))?;
    Ok(AdapterOutput::image(
        download_video(page, video_url, request.wait_timeout_ms.max(1)).await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::client::mock::MockPage;

    #[test]
    fn metadata_and_create_response_match_legacy_manifest() {
        assert_eq!(SoraAdapter.id(), "sora");
        assert_eq!(SoraAdapter.target_url(), TARGET_URL);
        assert_eq!(SoraAdapter.model_ids(), &["sora-2"]);
        assert_eq!(task_id_from_create(&json!({"id":"task-1"})), Some("task-1"));
        assert_eq!(task_id_from_create(&json!({"task_id":"task-1"})), None);
    }

    #[test]
    fn matches_pending_and_drafts_shapes_from_sora_api() {
        let pending = json!([{"id":"task-1","status":"running"}]);
        assert!(task_is_pending(&pending, "task-1"));
        assert!(!task_is_pending(&pending, "task-2"));
        let drafts = json!({"items":[{"task_id":"task-1","url":"https://cdn/video.mp4"}]});
        assert_eq!(
            video_url_from_drafts(&drafts, "task-1"),
            Some("https://cdn/video.mp4")
        );
        assert_eq!(video_url_from_drafts(&drafts, "missing"), None);
        let legacy_array = json!([{"task_id":"task-1","url":"https://cdn/video.mp4"}]);
        assert_eq!(
            video_url_from_drafts(&legacy_array, "task-1"),
            Some("https://cdn/video.mp4")
        );
    }

    #[tokio::test]
    async fn rejects_unknown_model_without_page_calls() {
        let page = MockPage::default();
        let request = GenerateRequest {
            prompt: "x".into(),
            model_id: "sora-1".into(),
            image_paths: vec![],
            reasoning: false,
            wait_timeout_ms: 1,
            adapter_config: Value::Null,
        };
        let result = SoraAdapter.generate(&page, &request).await.unwrap();
        assert_eq!(result.error.as_deref(), Some("未找到模型配置: sora-1"));
        assert!(page.calls.lock().unwrap().is_empty());
    }
}
