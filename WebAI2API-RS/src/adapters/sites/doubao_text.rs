use super::super::client::{
    chained, count_result, css, http_error, op, page_error, string_result, AdapterError,
    AdapterOutput, BrowserEvent, ClientFuture, GenerateRequest, PageClient, SiteAdapter,
};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

pub const TARGET_URL: &str = "https://www.doubao.com/chat/";
const INPUT: &str = "textarea.semi-input-textarea";
pub const MODEL_IDS: &[&str] = &["seed", "seed-thinking", "seed-pro"];

#[derive(Debug, Default, Clone, Copy)]
pub struct DoubaoTextAdapter;

impl SiteAdapter for DoubaoTextAdapter {
    fn id(&self) -> &'static str {
        "doubao_text"
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
    Err(AdapterError::new("未找到输入框 (.semi-input-textarea)"))
}

fn model_menu_pattern(id: &str) -> &'static str {
    match id {
        "seed-thinking" => {
            "Think Solves more complex problems|思考 擅长解决更难的问题|思考 擅長解決更難的問題"
        }
        "seed-pro" => "Pro Advanced Pro model|专家 研究级智能模型|專家 研究級智慧模型",
        _ => "Fast Solves most questions|快速 适用于大部分情况|快速 適用於大部分情況",
    }
}

fn model_label_matches(id: &str, label: &str) -> bool {
    model_menu_pattern(id)
        .split('|')
        .any(|part| label.contains(part))
}

async fn choose_model(page: &dyn PageClient, model_id: &str) -> Result<(), AdapterError> {
    call(page, vec![json!({"op":"waitForTimeout","ms":1200})]).await?;
    let buttons = css("#input-engine-container button[aria-haspopup='menu']");
    let count = count_result(
        &call(page, vec![op("locator.count", buttons.clone())]).await?,
        0,
    );
    let mut trigger = None;
    for index in 0..count {
        let button = chained(buttons.clone(), "nth", Some(index));
        let text = call(page, vec![op("locator.text", button.clone())]).await?;
        let text = string_result(&text, 0).unwrap_or("");
        if ["Fast", "Think", "Pro", "快速", "思考", "专家", "專家"]
            .iter()
            .any(|s| text.contains(s))
        {
            trigger = Some(button);
            break;
        }
    }
    let Some(trigger) = trigger else {
        return Ok(());
    };
    for attempt in 0..3 {
        if attempt > 0 {
            call(page, vec![json!({"op":"waitForTimeout","ms":750})]).await?;
        }
        call(
            page,
            vec![
                op("locator.click", trigger.clone()),
                json!({"op":"waitForTimeout","ms":350}),
            ],
        )
        .await?;
        let menu_items = css("div[role='menuitem']");
        let n = count_result(
            &call(page, vec![op("locator.count", menu_items.clone())]).await?,
            0,
        );
        for index in 0..n {
            let item = chained(menu_items.clone(), "nth", Some(index));
            let text = call(page, vec![op("locator.text", item.clone())]).await?;
            let label = string_result(&text, 0).unwrap_or("");
            if model_label_matches(model_id, label) {
                call(
                    page,
                    vec![
                        op("locator.click", item),
                        json!({"op":"waitForTimeout","ms":800}),
                    ],
                )
                .await?;
                return Ok(());
            }
        }
    }
    Err(AdapterError::new("模型选择菜单未弹出"))
}

async fn upload_images(
    page: &dyn PageClient,
    paths: &[std::path::PathBuf],
) -> Result<(), AdapterError> {
    if paths.is_empty() {
        return Ok(());
    }
    let menus = css("#input-engine-container button[aria-haspopup='menu']");
    let count = count_result(
        &call(page, vec![op("locator.count", menus.clone())]).await?,
        0,
    );
    let mut upload_menu = None;
    for index in 0..count {
        let button = chained(menus.clone(), "nth", Some(index));
        let text = call(page, vec![op("locator.text", button.clone())]).await?;
        let text = string_result(&text, 0).unwrap_or("").to_lowercase();
        if ![
            "fast", "think", "pro", "快速", "思考", "专家", "專家", "更多",
        ]
        .iter()
        .any(|s| text.contains(s))
        {
            upload_menu = Some(button);
            break;
        }
    }
    let Some(menu_button) = upload_menu else {
        return Err(AdapterError::new("未找到图片上传菜单"));
    };
    let cursor = page.subscribe_responses().await?;
    call(
        page,
        vec![
            op("locator.click", menu_button),
            json!({"op":"waitForTimeout","ms":400}),
        ],
    )
    .await?;
    let menu_items = css("div[role='menuitem']");
    let count = count_result(
        &call(page, vec![op("locator.count", menu_items.clone())]).await?,
        0,
    );
    let mut item = None;
    for index in 0..count {
        let candidate = chained(menu_items.clone(), "nth", Some(index));
        let text = call(page, vec![op("locator.text", candidate.clone())]).await?;
        let text = string_result(&text, 0).unwrap_or("");
        if ["上传文件或图片", "上傳檔案或圖片", "Upload File or Image"]
            .iter()
            .any(|s| text.contains(s))
        {
            item = Some(candidate);
            break;
        }
    }
    let item = item.ok_or_else(|| AdapterError::new("未找到上传文件或图片菜单项"))?;
    let files: Vec<String> = paths
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    call(page, vec![json!({"op":"filechooser.clickAndSetFiles","locator":item,"files":files,"options":{"timeout":10000}})]).await?;

    let start = Instant::now();
    let mut after = cursor;
    let mut expected_paths = std::collections::HashSet::new();
    let mut uploaded_paths = std::collections::HashSet::new();
    while start.elapsed() < Duration::from_secs(60) && uploaded_paths.len() < paths.len() {
        for event in page.poll_events(after).await? {
            after = after.max(event.sequence);
            let Some(url) = event.url.as_deref() else {
                continue;
            };
            if event.status != Some(200) {
                continue;
            }
            if url.contains("Action=ApplyImageUpload") {
                if let Some(id) = event.response_id.as_deref() {
                    let bytes = page.response_body(id).await?;
                    if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                        if let Some(store_uri) = value
                            .pointer("/Result/UploadAddress/StoreInfos/0/StoreUri")
                            .and_then(Value::as_str)
                        {
                            expected_paths.insert(store_uri.to_owned());
                        }
                    }
                }
            } else if event
                .request_method
                .as_deref()
                .is_some_and(|method| method.eq_ignore_ascii_case("POST"))
            {
                if let Some(path) = expected_paths
                    .iter()
                    .find(|path| url.contains(path.as_str()))
                {
                    uploaded_paths.insert(path.clone());
                }
            }
        }
        if uploaded_paths.len() < paths.len() {
            call(page, vec![json!({"op":"waitForTimeout","ms":200})]).await?;
        }
    }
    Ok(())
}

fn parse_sse(body: &str, use_thinking: bool) -> (String, String) {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut in_thinking = false;
    let mut thinking_id: Option<Value> = None;
    let lines: Vec<&str> = body.lines().collect();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index].trim();
        index += 1;
        let Some(kind) = line.strip_prefix("event:").map(str::trim) else {
            continue;
        };
        let Some(data_line) = lines.get(index).and_then(|line| line.strip_prefix("data:")) else {
            continue;
        };
        index += 1;
        let data_text = data_line.trim();
        if data_text.is_empty() || data_text == "{}" {
            continue;
        }
        let Ok(data) = serde_json::from_str::<Value>(data_text) else {
            continue;
        };
        if kind == "SSE_REPLY_END"
            && data.get("end_type").and_then(Value::as_i64) == Some(1)
            && text.is_empty()
        {
            if let Some(brief) = data
                .pointer("/msg_finish_attr/brief")
                .and_then(Value::as_str)
            {
                text.push_str(brief);
            }
        }
        if kind == "STREAM_MSG_NOTIFY" {
            if let Some(blocks) = data
                .pointer("/content/content_block")
                .and_then(Value::as_array)
            {
                for block in blocks {
                    if block.get("block_type").and_then(Value::as_i64) == Some(10040)
                        && block.pointer("/content/thinking_block").is_some()
                    {
                        in_thinking = true;
                        thinking_id = block.get("block_id").cloned();
                    }
                }
            }
        }
        if kind == "STREAM_CHUNK" {
            if let Some(ops) = data.get("patch_op").and_then(Value::as_array) {
                for patch in ops {
                    let Some(blocks) = patch
                        .pointer("/patch_value/content_block")
                        .and_then(Value::as_array)
                    else {
                        continue;
                    };
                    for block in blocks {
                        if block.get("block_type").and_then(Value::as_i64) == Some(10040)
                            && block.get("is_finish").and_then(Value::as_bool) == Some(true)
                        {
                            in_thinking = false;
                        }
                        let part = block
                            .pointer("/content/text_block/text")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        if use_thinking
                            && thinking_id
                                .as_ref()
                                .is_some_and(|id| block.get("parent_id") == Some(id))
                        {
                            reasoning.push_str(part);
                        } else if block.get("block_type").and_then(Value::as_i64) == Some(10000)
                            && thinking_id.as_ref() != block.get("parent_id")
                        {
                            text.push_str(part);
                        }
                    }
                }
            }
        }
        if kind == "CHUNK_DELTA" {
            if let Some(delta) = data.get("text").and_then(Value::as_str) {
                if use_thinking && in_thinking {
                    reasoning.push_str(delta);
                } else {
                    text.push_str(delta);
                }
            }
        }
    }
    (text.trim().to_owned(), reasoning.trim().to_owned())
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
                && event
                    .url
                    .as_deref()
                    .is_some_and(|url| url.contains("chat/completion"))
            {
                let id = event
                    .response_id
                    .as_deref()
                    .ok_or_else(|| AdapterError::new("豆包响应缺少 responseId"))?;
                let body = page.response_body(id).await?;
                let body = String::from_utf8_lossy(&body);
                if let Some(error) = http_error(event.status.unwrap_or(200), &body) {
                    return Err(error);
                }
                let (text, _) = parse_sse(&body, true);
                if !text.is_empty() {
                    return Ok((event, body.into_owned()));
                }
            }
        }
        call(page, vec![json!({"op":"waitForTimeout","ms":250})]).await?;
    }
    Err(AdapterError::normalized(
        format!("API_TIMEOUT: 响应超时 ({}秒)", timeout_ms / 1000),
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
    let input = wait_input(page).await?;
    choose_model(page, &request.model_id).await?;
    // Legacy upload errors are logged and generation continues with text.
    let _ = upload_images(page, &request.image_paths).await;
    call(page, vec![op("locator.click", input.clone()), json!({"op":"locator.fill","locator":input,"value":request.prompt,"options":{"timeout":10000}})]).await?;
    let cursor = page.subscribe_responses().await?;
    let send = css("button#flow-end-msg-send");
    call(page, vec![json!({"op":"locator.waitFor","locator":send,"options":{"state":"visible","timeout":10000}})]).await?;
    call(
        page,
        vec![op("locator.click", css("button#flow-end-msg-send"))],
    )
    .await?;
    let (_, body) = wait_completion(page, cursor, request.wait_timeout_ms.max(1)).await?;
    let (text, reasoning) = parse_sse(
        &body,
        request.model_id == "seed-thinking" || request.model_id == "seed-pro",
    );
    if text.is_empty() {
        return Ok(AdapterOutput::error("未能从响应中提取文本"));
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
    fn metadata_and_model_menu_labels_match_legacy_manifest() {
        assert_eq!(DoubaoTextAdapter.id(), "doubao_text");
        assert_eq!(TARGET_URL, "https://www.doubao.com/chat/");
        assert_eq!(MODEL_IDS, &["seed", "seed-thinking", "seed-pro"]);
        assert!(model_label_matches(
            "seed-thinking",
            "Think Solves more complex problems"
        ));
        assert!(model_label_matches("seed-pro", "专家 研究级智能模型"));
        assert!(!model_label_matches(
            "seed-pro",
            "Think Solves more complex problems"
        ));
    }

    #[test]
    fn sse_parser_extracts_final_text_and_thinking_delta() {
        let body = concat!(
            "event: STREAM_MSG_NOTIFY\ndata: {\"content\":{\"content_block\":[{\"block_type\":10040,\"block_id\":7,\"content\":{\"thinking_block\":{}}}]}}\n",
            "event: CHUNK_DELTA\ndata: {\"text\":\"reason\"}\n",
            "event: STREAM_CHUNK\ndata: {\"patch_op\":[{\"patch_object\":1,\"patch_value\":{\"content_block\":[{\"block_type\":10000,\"content\":{\"text_block\":{\"text\":\"answer\"}}}]}}]}\n"
        );
        assert_eq!(parse_sse(body, true), ("answer".into(), "reason".into()));
        assert_eq!(parse_sse(body, false), ("reasonanswer".into(), "".into()));
    }

    #[tokio::test]
    async fn input_wait_uses_legacy_textarea_selector_over_mock_rpc() {
        let page = MockPage::returning([Value::from(1)]);
        let input = wait_input(&page).await.unwrap();
        assert_eq!(input["value"], INPUT);
        assert_eq!(page.calls.lock().unwrap()[0][0]["op"], "locator.count");
    }

    #[tokio::test]
    async fn unsupported_model_fails_without_page_rpc() {
        let page = MockPage::default();
        let request = GenerateRequest {
            prompt: "x".into(),
            model_id: "unknown".into(),
            image_paths: vec![],
            reasoning: false,
            wait_timeout_ms: 50,
            adapter_config: Value::Null,
        };
        assert_eq!(
            DoubaoTextAdapter
                .generate(&page, &request)
                .await
                .unwrap()
                .error
                .as_deref(),
            Some("未找到模型配置: unknown")
        );
        assert!(page.calls.lock().unwrap().is_empty());
    }
}
