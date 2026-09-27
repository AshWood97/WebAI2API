use super::super::client::{
    chained, count_result, http_error, op, page_error, role, AdapterError, AdapterOutput,
    BrowserEvent, ClientFuture, GenerateRequest, PageClient, SiteAdapter,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

pub const TARGET_URL: &str = "https://gemini.google.com/app?hl=en";
pub const MODEL_IDS: &[&str] = &[
    "gemini-3.1-flash",
    "gemini-3.1-flash-thinking",
    "gemini-3.1-pro",
];

#[derive(Debug, Default, Clone, Copy)]
pub struct GeminiTextAdapter;

impl SiteAdapter for GeminiTextAdapter {
    fn id(&self) -> &'static str {
        "gemini_text"
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
            if event.event_type == "response"
                && event.url.as_deref().is_some_and(|url| {
                    url.contains("assistant.lamda.BardFrontendService/StreamGenerate")
                })
                && event
                    .request_method
                    .as_deref()
                    .is_some_and(|method| method.eq_ignore_ascii_case("POST"))
            {
                return Ok(event);
            }
        }
        call(page, vec![json!({"op":"waitForTimeout","ms":200})]).await?;
    }
    Err(AdapterError::normalized(
        "API_TIMEOUT: 等待 Gemini StreamGenerate 响应超时",
        "TIMEOUT_ERROR",
        true,
    ))
}

fn temporary_chat(request: &GenerateRequest) -> bool {
    request
        .adapter_config
        .get("temporaryChat")
        .and_then(Value::as_bool)
        .or_else(|| {
            request
                .adapter_config
                .pointer("/backend/adapter/gemini_text/temporaryChat")
                .and_then(Value::as_bool)
        })
        .unwrap_or(false)
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
    let mut after = cursor;
    let started = Instant::now();
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
                && url.contains("google.com/upload/")
                && url.contains("upload_id=")
            {
                if event.status == Some(200) {
                    return Ok(());
                }
                return Err(http_error(event.status.unwrap_or(500), "upload failed")
                    .unwrap_or_else(|| AdapterError::new("Gemini 图片上传失败")));
            }
        }
        call(page, vec![json!({"op":"waitForTimeout","ms":200})]).await?;
    }
    Err(AdapterError::normalized(
        "API_TIMEOUT: Gemini 图片上传超时",
        "TIMEOUT_ERROR",
        true,
    ))
}

async fn choose_model(page: &dyn PageClient, model_id: &str) -> Result<(), AdapterError> {
    let picker = role("button", "Open mode picker");
    wait_visible(page, picker.clone(), 5_000).await?;
    call(page, vec![op("locator.click", picker)]).await?;
    call(page, vec![json!({"op":"waitForTimeout","ms":350})]).await?;
    let menu = role("menuitem", "");
    let count = count_result(
        &call(page, vec![op("locator.count", menu.clone())]).await?,
        0,
    );
    if count == 0 {
        return Ok(());
    }
    let mut texts = Vec::with_capacity(count);
    for index in 0..count {
        let item = chained(menu.clone(), "nth", Some(index));
        texts.push(
            call(page, vec![op("locator.text", item)])
                .await?
                .first()
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_owned(),
        );
    }
    let has_pro = texts.iter().any(|text| text.starts_with("Pro"));
    let wanted = if has_pro {
        match model_id {
            "gemini-3.1-pro" => "Pro",
            "gemini-3.1-flash-thinking" => "Thinking",
            _ => "Fast",
        }
    } else if matches!(model_id, "gemini-3.1-pro" | "gemini-3.1-flash-thinking") {
        "Thinking"
    } else {
        "Fast"
    };
    if let Some(index) = texts.iter().position(|text| text.starts_with(wanted)) {
        call(
            page,
            vec![op("locator.click", chained(menu, "nth", Some(index)))],
        )
        .await?;
    } else {
        let _ = call(page, vec![json!({"op":"keyboard.press","key":"Escape"})]).await;
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
    let input = json!({"kind":"role","role":"textbox"});
    wait_visible(page, input.clone(), 30_000).await?;
    for path in &request.image_paths {
        upload_file(page, path, request.wait_timeout_ms).await?;
    }
    choose_model(page, &request.model_id).await?;
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
    let response = wait_response(page, cursor, request.wait_timeout_ms).await?;
    let id = response
        .response_id
        .as_deref()
        .ok_or_else(|| AdapterError::new("Gemini 响应缺少 responseId"))?;
    page.response_wait_finished(id, request.wait_timeout_ms.max(1))
        .await?;
    let body = page.response_body(id).await?;
    if let Some(error) = http_error(
        response.status.unwrap_or(200),
        &String::from_utf8_lossy(&body),
    ) {
        return Err(AdapterError::new(format!(
            "API 返回错误: {}",
            error.message
        )));
    }
    let payloads = extract_payloads(&body)?;
    let (text, reasoning) = extract_final_text_and_thinking(&payloads);
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
        for item in frame
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_array)
        {
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

fn collect_rc_texts(payload: &Value) -> HashMap<String, String> {
    let mut best_by_rc: HashMap<String, String> = HashMap::new();
    visit_values(std::slice::from_ref(payload), |value| {
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
        if !text.is_empty() && text.len() >= best_by_rc.get(key).map_or(0, String::len) {
            best_by_rc.insert(key.to_owned(), text);
        }
    });
    best_by_rc
}

fn extract_text_and_thinking(payload: &Value) -> (String, String) {
    let Some(root) = payload.as_array() else {
        return (String::new(), String::new());
    };
    let Some(rc) = root
        .get(4)
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(Value::as_array)
        .filter(|rc| {
            rc.first()
                .and_then(Value::as_str)
                .is_some_and(|key| key.starts_with("rc_"))
        })
    else {
        return (String::new(), String::new());
    };
    let text = rc
        .get(1)
        .and_then(Value::as_array)
        .and_then(|parts| parts.first())
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let thinking = rc
        .get(37)
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(Value::as_array)
        .and_then(|parts| parts.first())
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    (text, thinking)
}

fn extract_final_text_and_thinking(payloads: &[Value]) -> (String, String) {
    let mut best = (String::new(), String::new());
    for payload in payloads {
        let result = extract_text_and_thinking(payload);
        if result.0.len() > best.0.len() {
            best = result;
        }
    }
    if best.0.is_empty() {
        for payload in payloads {
            for text in collect_rc_texts(payload).into_values() {
                if text.len() > best.0.len() {
                    best.0 = text;
                }
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::client::mock::MockPage;

    fn request(model_id: &str) -> GenerateRequest {
        GenerateRequest {
            prompt: "hello".into(),
            model_id: model_id.into(),
            image_paths: vec![],
            reasoning: false,
            wait_timeout_ms: 1,
            adapter_config: Value::Null,
        }
    }

    #[test]
    fn metadata_matches_legacy_manifest() {
        assert_eq!(GeminiTextAdapter.id(), "gemini_text");
        assert_eq!(GeminiTextAdapter.target_url(), TARGET_URL);
        assert_eq!(
            MODEL_IDS,
            [
                "gemini-3.1-flash",
                "gemini-3.1-flash-thinking",
                "gemini-3.1-pro"
            ]
        );
    }

    #[tokio::test]
    async fn unsupported_model_does_not_touch_page() {
        let page = MockPage::default();
        let result = GeminiTextAdapter
            .generate(&page, &request("unknown"))
            .await
            .unwrap();
        assert_eq!(result.error.as_deref(), Some("未找到模型配置: unknown"));
        assert!(page.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn parses_text_and_thinking_from_batchexecute_contract() {
        let mut rc = vec![Value::Null; 38];
        rc[0] = json!("rc_answer");
        rc[1] = json!(["final answer"]);
        rc[37] = json!([["reasoning trace"]]);
        let mut payload = vec![Value::Null; 5];
        payload[4] = json!([rc]);
        let wrapped = json!([["wrb.fr", null, Value::Array(payload).to_string()]]).to_string();
        let body = format!(")]}}'\n{}\n{}", wrapped.len(), wrapped);
        let payloads = extract_payloads(body.as_bytes()).unwrap();
        assert_eq!(
            extract_final_text_and_thinking(&payloads),
            ("final answer".into(), "reasoning trace".into())
        );
    }

    #[test]
    fn malformed_truncated_frame_is_reported() {
        assert!(parse_len_framed_response(b"4\n[{")
            .unwrap_err()
            .message
            .contains("截断"));
    }
}
