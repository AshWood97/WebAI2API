use super::super::client::{
    chained, count_result, css, http_error, op, page_error, role, string_result, AdapterError,
    AdapterOutput, BrowserEvent, ClientFuture, GenerateRequest, PageClient, SiteAdapter,
};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

pub const TARGET_URL: &str = "https://zai.is/";
const INPUT: &str = ".tiptap.ProseMirror";
pub const MODEL_IDS: &[&str] = &[
    "glm-4.6",
    "gemini-3-pro-preview",
    "gemini-2.5-pro",
    "gemini-3-flash-preview",
    "claude-sonnet-4.5",
    "claude-sonnet-4",
    "claude-haiku-4.5",
    "gpt-5.1",
    "gpt-5",
    "gpt-4.1",
    "gpt-5.2",
    "o3-high",
    "o3-mini",
    "o4-mini",
    "grok-4.1-fast",
    "grok-4",
    "kimi-k2-thinking",
];

#[derive(Debug, Default, Clone, Copy)]
pub struct ZaiIsTextAdapter;

impl SiteAdapter for ZaiIsTextAdapter {
    fn id(&self) -> &'static str {
        "zai_is_text"
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

fn code_name(model: &str) -> &str {
    match model {
        "glm-4.6" => "GLM 4.6",
        "gemini-3-pro-preview" => "Gemini 3 Pro Preview",
        "gemini-2.5-pro" => "Gemini 2.5 Pro",
        "gemini-3-flash-preview" => "Gemini 3 Flash Preview",
        "claude-sonnet-4.5" => "Claude Sonnet 4.5",
        "claude-sonnet-4" => "Claude Sonnet 4",
        "claude-haiku-4.5" => "Claude Haiku 4.5",
        "gpt-5.1" => "GPT-5.1",
        "gpt-5" => "GPT-5",
        "gpt-4.1" => "GPT-4.1",
        "gpt-5.2" => "GPT-5.2 Chat",
        "o3-high" => "o3-high",
        "o3-mini" => "o3-mini",
        "o4-mini" => "o4-mini",
        "grok-4.1-fast" => "Grok 4.1 Fast",
        "grok-4" => "Grok 4",
        "kimi-k2-thinking" => "Kimi K2 Thinking",
        _ => model,
    }
}

async fn call(page: &dyn PageClient, operations: Vec<Value>) -> Result<Vec<Value>, AdapterError> {
    page.call(operations).await
}

async fn wait_url(
    page: &dyn PageClient,
    predicate: impl Fn(&str) -> bool,
    timeout_ms: u64,
) -> Result<String, AdapterError> {
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(timeout_ms) {
        let values = call(page, vec![json!({"op":"url"})]).await?;
        let url = string_result(&values, 0).unwrap_or("").to_owned();
        if predicate(&url) {
            return Ok(url);
        }
        call(page, vec![json!({"op":"waitForTimeout","ms":250})]).await?;
    }
    Err(AdapterError::normalized(
        "API_TIMEOUT: 等待 Discord OAuth 页面跳转超时",
        "TIMEOUT_ERROR",
        true,
    ))
}

async fn handle_discord_auth(page: &dyn PageClient) -> Result<(), AdapterError> {
    let url = call(page, vec![json!({"op":"url"})]).await?;
    if !string_result(&url, 0).unwrap_or("").contains("zai.is/auth") {
        return Ok(());
    }
    let buttons = css("button");
    call(page, vec![json!({"op":"locator.waitFor","locator":buttons,"options":{"state":"visible","timeout":30000}}), op("locator.click", css("button"))]).await?;
    wait_url(
        page,
        |url| url.contains("discord.com/oauth2/authorize"),
        60_000,
    )
    .await?;
    let authorize = css("div[data-align='stretch'] button:last-child");
    let main = css("main");
    let start = Instant::now();
    let mut clicked = false;
    while start.elapsed() < Duration::from_secs(30) {
        let enabled = call(
            page,
            vec![json!({"op":"locator.enabled","locator":authorize})],
        )
        .await
        .ok()
        .and_then(|v| v.first().and_then(Value::as_bool))
        .unwrap_or(false);
        if enabled {
            call(
                page,
                vec![
                    json!({"op":"waitForTimeout","ms":400}),
                    op("locator.click", authorize.clone()),
                ],
            )
            .await?;
            clicked = true;
            break;
        }
        // A generic scroll primitive is needed here for Discord's consent layout.
        call(
            page,
            vec![
                json!({"op":"locator.scrollIntoViewIfNeeded","locator":main.clone()}),
                json!({"op":"waitForTimeout","ms":250}),
            ],
        )
        .await?;
    }
    if !clicked {
        return Err(AdapterError::new("Discord 授权按钮不可用"));
    }
    wait_url(
        page,
        |url| url.contains("zai.is") && !url.contains("/auth") && !url.contains("discord.com"),
        60_000,
    )
    .await?;
    call(page, vec![json!({"op":"waitForTimeout","ms":750})]).await?;
    Ok(())
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
    Err(AdapterError::new("未找到输入框 (.tiptap.ProseMirror)"))
}

async fn select_model(page: &dyn PageClient, model: &str) -> Result<(), AdapterError> {
    let select = role("button", "Select a model");
    call(page, vec![json!({"op":"locator.waitFor","locator":select,"options":{"state":"visible","timeout":5000}}), op("locator.click", role("button", "Select a model")), json!({"op":"waitForTimeout","ms":400})]).await?;
    let search = role("textbox", "Search In Models");
    call(page, vec![json!({"op":"locator.waitFor","locator":search,"options":{"state":"visible","timeout":5000}}), json!({"op":"locator.fill","locator":role("textbox", "Search In Models"),"value":code_name(model),"options":{"timeout":5000}}), json!({"op":"waitForTimeout","ms":350}), json!({"op":"locator.press","locator":role("textbox", "Search In Models"),"key":"Enter"})]).await?;
    Ok(())
}

async fn upload_files(
    page: &dyn PageClient,
    target: &Value,
    paths: &[std::path::PathBuf],
    response_match: &str,
) -> Result<(), AdapterError> {
    call(
        page,
        vec![
            op("locator.click", target.clone()),
            json!({"op":"waitForTimeout","ms":400}),
        ],
    )
    .await?;
    let inputs = css("input[type='file']");
    let count = count_result(
        &call(page, vec![op("locator.count", inputs.clone())]).await?,
        0,
    );
    if count == 0 {
        return Err(AdapterError::new(
            "未找到任何 input[type=\"file\"] 控件,无法上传",
        ));
    }
    let files: Vec<String> = paths
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    let cursor = page.subscribe_responses().await?;
    let mut uploaded = false;
    for index in 0..count {
        let input = chained(inputs.clone(), "nth", Some(index));
        if call(
            page,
            vec![json!({"op":"upload","locator":input,"files":files.clone()})],
        )
        .await
        .is_ok()
        {
            uploaded = true;
            break;
        }
    }
    if !uploaded {
        return Err(AdapterError::new("所有文件控件均无法接受输入"));
    }
    let start = Instant::now();
    let mut after = cursor;
    let mut matched = 0;
    while start.elapsed() < Duration::from_secs(60) && matched < paths.len() {
        for event in page.poll_events(after).await? {
            after = after.max(event.sequence);
            if event.event_type == "response"
                && event.status == Some(200)
                && event
                    .url
                    .as_deref()
                    .is_some_and(|url| url.contains(response_match))
            {
                matched += 1;
            }
        }
        if matched < paths.len() {
            call(page, vec![json!({"op":"waitForTimeout","ms":200})]).await?;
        }
    }
    Ok(())
}

async fn wait_api_response(
    page: &dyn PageClient,
    after: &mut u64,
    pending: &mut Vec<BrowserEvent>,
    url_match: &str,
    timeout_ms: u64,
) -> Result<BrowserEvent, AdapterError> {
    let start = Instant::now();
    loop {
        if let Some(index) = pending.iter().position(|event| {
            event.event_type == "response"
                && event
                    .request_method
                    .as_deref()
                    .is_some_and(|m| m.eq_ignore_ascii_case("POST"))
                && event
                    .url
                    .as_deref()
                    .is_some_and(|url| url.contains(url_match))
        }) {
            return Ok(pending.remove(index));
        }
        if start.elapsed() >= Duration::from_millis(timeout_ms) {
            break;
        }
        let events = page.poll_events(*after).await?;
        for event in events {
            *after = (*after).max(event.sequence);
            pending.push(event);
        }
        call(page, vec![json!({"op":"waitForTimeout","ms":200})]).await?;
    }
    Err(AdapterError::normalized(
        format!("API_TIMEOUT: 等待 {url_match} 响应超时"),
        "TIMEOUT_ERROR",
        true,
    ))
}

async fn response_json(page: &dyn PageClient, event: &BrowserEvent) -> Result<Value, AdapterError> {
    let id = event
        .response_id
        .as_deref()
        .ok_or_else(|| AdapterError::new("zAI 响应缺少 responseId"))?;
    let bytes = page.response_body(id).await?;
    let body = String::from_utf8_lossy(&bytes);
    if let Some(error) = http_error(event.status.unwrap_or(200), &body) {
        return Err(error);
    }
    serde_json::from_str(&body).map_err(|_| AdapterError::new("解析 zAI 响应失败"))
}

fn strip_reasoning_details(content: &str) -> String {
    let trimmed = content.trim_start();
    if let Some(rest) = trimmed.strip_prefix("<details type=\"reasoning\"") {
        if let Some(end) = rest.find("</details>") {
            return rest[end + "</details>".len()..].trim().to_owned();
        }
    }
    content.trim().to_owned()
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
    handle_discord_auth(page).await?;
    let input = wait_input(page).await?;
    if !request.image_paths.is_empty() {
        upload_files(page, &input, &request.image_paths, "v1/files").await?;
    }
    call(page, vec![op("locator.click", input.clone()), json!({"op":"locator.fill","locator":input,"value":request.prompt,"options":{"timeout":10000}})]).await?;
    select_model(page, &request.model_id).await?;
    let cursor = page.subscribe_responses().await?;
    call(
        page,
        vec![op("locator.click", css("button[type='submit']"))],
    )
    .await?;
    let mut after = cursor;
    let mut pending = Vec::new();
    let chats_new =
        wait_api_response(page, &mut after, &mut pending, "v1/chats/new", 60_000).await?;
    let chats_body = response_json(page, &chats_new).await?;
    if chats_body.get("id").is_none() {
        return Ok(AdapterOutput::error("创建对话响应中没有 id"));
    }

    let completion = wait_api_response(
        page,
        &mut after,
        &mut pending,
        "chat/completions",
        request.wait_timeout_ms.max(1),
    )
    .await?;
    let completion_body = response_json(page, &completion).await?;
    if completion_body.get("status").and_then(Value::as_bool) != Some(true) {
        return Ok(AdapterOutput::error("生成失败，响应状态异常"));
    }

    let completed = match wait_api_response(
        page,
        &mut after,
        &mut pending,
        "chat/completed",
        request.wait_timeout_ms.max(1),
    )
    .await
    {
        Ok(event) => event,
        Err(error) if error.message.starts_with("API_TIMEOUT:") => {
            return Ok(AdapterOutput::error("等待完成响应超时 (120秒)"))
        }
        Err(error) => return Err(error),
    };
    let body = response_json(page, &completed).await?;
    let id = body.get("id");
    let target = body
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| messages.iter().find(|msg| msg.get("id") == id));
    let Some(content) = target
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
    else {
        return Ok(AdapterOutput::error("未找到匹配的消息"));
    };
    if content.trim().is_empty() {
        return Ok(AdapterOutput::error("回复内容为空可能触发违规/限流"));
    }
    let text = strip_reasoning_details(content);
    if text.is_empty() {
        return Ok(AdapterOutput::error("提取文本内容为空"));
    }
    Ok(AdapterOutput::text(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::client::mock::MockPage;

    #[test]
    fn metadata_and_model_labels_match_legacy_manifest() {
        assert_eq!(ZaiIsTextAdapter.id(), "zai_is_text");
        assert_eq!(TARGET_URL, "https://zai.is/");
        assert_eq!(MODEL_IDS.len(), 17);
        assert_eq!(code_name("glm-4.6"), "GLM 4.6");
        assert_eq!(code_name("gpt-5.2"), "GPT-5.2 Chat");
    }

    #[test]
    fn removes_only_leading_reasoning_details_block() {
        assert_eq!(
            strip_reasoning_details("<details type=\"reasoning\" open>thinking</details>\nanswer"),
            "answer"
        );
        assert_eq!(
            strip_reasoning_details("answer <details>keep</details>"),
            "answer <details>keep</details>"
        );
    }

    #[tokio::test]
    async fn input_wait_uses_legacy_selector_over_mock_rpc() {
        let page = MockPage::returning([Value::from(1)]);
        let input = wait_input(&page).await.unwrap();
        assert_eq!(input["value"], INPUT);
        assert_eq!(page.calls.lock().unwrap()[0][0]["op"], "locator.count");
    }

    #[tokio::test]
    async fn unsupported_model_fails_without_browser_calls() {
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
            ZaiIsTextAdapter
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
