use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{future::Future, path::Path, pin::Pin};
use thiserror::Error;

pub type ClientFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, AdapterError>> + Send + 'a>>;

#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("{message}")]
pub struct AdapterError {
    pub message: String,
    pub code: Option<String>,
    pub retryable: Option<bool>,
}

impl AdapterError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: None,
            retryable: None,
        }
    }

    pub fn normalized(
        message: impl Into<String>,
        code: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self {
            message: message.into(),
            code: Some(code.into()),
            retryable: Some(retryable),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
}

impl AdapterOutput {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: Some(text.into()),
            reasoning: None,
            image: None,
            error: None,
            code: None,
            retryable: None,
        }
    }

    pub fn with_reasoning(mut self, reasoning: impl Into<String>) -> Self {
        self.reasoning = Some(reasoning.into());
        self
    }

    pub fn image(image: impl Into<String>) -> Self {
        Self {
            text: None,
            reasoning: None,
            image: Some(image.into()),
            error: None,
            code: None,
            retryable: None,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            text: None,
            reasoning: None,
            image: None,
            error: Some(message.into()),
            code: None,
            retryable: None,
        }
    }

    pub fn normalized_error(error: AdapterError) -> Self {
        Self {
            text: None,
            reasoning: None,
            image: None,
            error: Some(error.message),
            code: error.code,
            retryable: error.retryable,
        }
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::normalized_error(AdapterError::normalized(
            message,
            "ADAPTER_UNSUPPORTED",
            false,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BrowserEvent {
    pub sequence: u64,
    pub event_type: String,
    pub browser_id: Option<String>,
    pub page_id: Option<String>,
    pub generation: Option<u64>,
    pub response_id: Option<String>,
    pub url: Option<String>,
    pub status: Option<u16>,
    pub request_method: Option<String>,
    pub headers: Option<Value>,
    pub route_token: Option<String>,
    pub request: Option<Value>,
}

impl BrowserEvent {
    pub fn from_json(value: &Value) -> Option<Self> {
        Some(Self {
            sequence: value.get("sequence")?.as_u64()?,
            event_type: value.get("type")?.as_str()?.to_owned(),
            browser_id: value
                .get("browserId")
                .and_then(Value::as_str)
                .map(str::to_owned),
            page_id: value
                .get("pageId")
                .and_then(Value::as_str)
                .map(str::to_owned),
            generation: value.get("generation").and_then(Value::as_u64),
            response_id: value
                .get("responseId")
                .and_then(Value::as_str)
                .map(str::to_owned),
            url: value.get("url").and_then(Value::as_str).map(str::to_owned),
            status: value
                .get("status")
                .and_then(Value::as_u64)
                .and_then(|v| u16::try_from(v).ok()),
            request_method: value
                .get("method")
                .and_then(Value::as_str)
                .map(str::to_owned),
            headers: value.get("headers").cloned(),
            route_token: value
                .get("routeToken")
                .and_then(Value::as_str)
                .map(str::to_owned),
            request: value.get("request").cloned(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenerateRequest {
    pub prompt: String,
    pub model_id: String,
    #[serde(default)]
    pub image_paths: Vec<std::path::PathBuf>,
    #[serde(default)]
    pub reasoning: bool,
    pub wait_timeout_ms: u64,
    #[serde(default)]
    pub adapter_config: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DownloadResult {
    pub bytes: u64,
    pub content_type: Option<String>,
}

/// A page bound to a generic browser runtime page ID.
///
/// `call` accepts the same `{ op, ... }` objects as `page.call` in the browser
/// runtime and returns the operation results in order. This trait is mockable,
/// allowing adapter contracts to run without a browser or credentials.
pub trait PageClient: Send + Sync {
    fn call<'a>(&'a self, operations: Vec<Value>) -> ClientFuture<'a, Vec<Value>>;
    fn subscribe_responses<'a>(&'a self) -> ClientFuture<'a, u64>;
    fn poll_events<'a>(&'a self, after_sequence: u64) -> ClientFuture<'a, Vec<BrowserEvent>>;
    /// Return raw response bytes. BrowserRuntime transports them as base64 and
    /// the concrete RPC client owns that transport conversion.
    fn response_body<'a>(&'a self, response_id: &'a str) -> ClientFuture<'a, Vec<u8>>;

    /// Wait until a captured response's request has finished or failed. A
    /// failed request or timeout should be returned as `AdapterError` by the
    /// concrete runtime client.
    fn response_wait_finished<'a>(
        &'a self,
        _response_id: &'a str,
        _timeout_ms: u64,
    ) -> ClientFuture<'a, ()> {
        Box::pin(async {
            Err(AdapterError::new(
                "browser RPC response.waitFinished is unsupported",
            ))
        })
    }

    /// Fetch response bytes to a file. Production RPC clients should override
    /// this and use `response.body` with its `path` parameter to avoid keeping
    /// large media payloads in Rust memory. The fallback is convenient for
    /// mocks and small bodies only.
    fn response_body_file<'a>(
        &'a self,
        response_id: &'a str,
        path: &'a Path,
    ) -> ClientFuture<'a, u64> {
        Box::pin(async move {
            let bytes = self.response_body(response_id).await?;
            tokio::fs::write(path, &bytes)
                .await
                .map_err(|error| AdapterError::new(format!("write response body: {error}")))?;
            Ok(bytes.len() as u64)
        })
    }

    fn route_install<'a>(
        &'a self,
        _pattern: &'a str,
        _timeout_ms: u64,
    ) -> ClientFuture<'a, String> {
        Box::pin(async {
            Err(AdapterError::new(
                "browser RPC route.install is unsupported",
            ))
        })
    }
    fn route_resolve<'a>(
        &'a self,
        _route_token: &'a str,
        _decision: Value,
    ) -> ClientFuture<'a, ()> {
        Box::pin(async {
            Err(AdapterError::new(
                "browser RPC route.resolve is unsupported",
            ))
        })
    }
    fn route_remove<'a>(&'a self, _route_id: &'a str) -> ClientFuture<'a, ()> {
        Box::pin(async { Err(AdapterError::new("browser RPC route.remove is unsupported")) })
    }
    fn download_fetch<'a>(
        &'a self,
        _url: &'a str,
        _path: &'a Path,
        _headers: Value,
        _timeout_ms: u64,
    ) -> ClientFuture<'a, u64> {
        Box::pin(async {
            Err(AdapterError::new(
                "browser RPC download.fetch is unsupported",
            ))
        })
    }
    fn download_fetch_info<'a>(
        &'a self,
        url: &'a str,
        path: &'a Path,
        headers: Value,
        timeout_ms: u64,
    ) -> ClientFuture<'a, DownloadResult> {
        Box::pin(async move {
            let bytes = self.download_fetch(url, path, headers, timeout_ms).await?;
            Ok(DownloadResult {
                bytes,
                content_type: None,
            })
        })
    }
    fn cookies_get<'a>(&'a self, _urls: Vec<String>) -> ClientFuture<'a, Vec<Value>> {
        Box::pin(async { Err(AdapterError::new("browser RPC cookies.get is unsupported")) })
    }
}

pub trait SiteAdapter: Send + Sync {
    fn id(&self) -> &'static str;
    fn target_url(&self) -> &'static str;
    fn model_ids(&self) -> &'static [&'static str];

    fn generate<'a>(
        &'a self,
        page: &'a dyn PageClient,
        request: &'a GenerateRequest,
    ) -> ClientFuture<'a, AdapterOutput>;
}

pub(crate) fn css(selector: &str) -> Value {
    serde_json::json!({"kind":"css", "value":selector})
}

pub(crate) fn role(role: &str, name: &str) -> Value {
    serde_json::json!({"kind":"role", "role":role, "name":name})
}

pub(crate) fn chained(mut locator: Value, op: &str, index: Option<usize>) -> Value {
    let chain = locator
        .as_object_mut()
        .unwrap()
        .entry("chain")
        .or_insert_with(|| Value::Array(Vec::new()));
    let mut step = serde_json::json!({"op":op});
    if let Some(index) = index {
        step["index"] = Value::from(index);
    }
    chain.as_array_mut().unwrap().push(step);
    locator
}

pub(crate) fn op(name: &str, locator: Value) -> Value {
    serde_json::json!({"op":name, "locator":locator})
}

pub(crate) fn string_result(values: &[Value], index: usize) -> Option<&str> {
    values.get(index).and_then(Value::as_str)
}

pub(crate) fn count_result(values: &[Value], index: usize) -> usize {
    values
        .get(index)
        .and_then(Value::as_u64)
        .unwrap_or_default() as usize
}

pub(crate) fn page_error(message: &str) -> Option<AdapterError> {
    let exact = match message {
        "PAGE_CLOSED" => Some(("页面已关闭，请勿在生图过程中刷新页面", "PAGE_CLOSED", true)),
        "PAGE_CRASHED" => Some(("页面崩溃，请重试", "PAGE_CRASHED", true)),
        "PAGE_INVALID" => Some(("页面状态无效，请重新初始化", "PAGE_INVALID", true)),
        _ => None,
    };
    if let Some((msg, code, retryable)) = exact {
        return Some(AdapterError::normalized(msg, code, retryable));
    }
    if message.starts_with("API_TIMEOUT:")
        || message.contains("页面加载超时")
        || message.contains("页面加载失败")
    {
        return Some(AdapterError::normalized(message, "TIMEOUT_ERROR", true));
    }
    if message.starts_with("PAGE_ERROR_DETECTED:") || message.starts_with("API_ERROR_DETECTED:") {
        let keyword = message
            .split_once(':')
            .map(|(_, value)| value.trim())
            .unwrap_or(message);
        return Some(AdapterError::normalized(
            format!("内容被阻止: {keyword}"),
            "CONTENT_BLOCKED",
            false,
        ));
    }
    None
}

pub(crate) fn http_error(status: u16, body: &str) -> Option<AdapterError> {
    let detail = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v.get("error").and_then(|e| {
                e.as_str()
                    .map(str::to_owned)
                    .or_else(|| e.get("message").and_then(Value::as_str).map(str::to_owned))
            })
        })
        .or_else(|| (body.len() < 200 && !body.trim().is_empty()).then(|| body.to_owned()));
    if let Some(detail) = detail.as_deref().filter(|text| {
        let lower = text.to_ascii_lowercase();
        [
            "reject",
            "violat",
            "terms",
            "blocked",
            "forbidden",
            "unsafe",
            "moderat",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
            || *text == "prompt failed"
    }) {
        return Some(AdapterError::normalized(
            format!("内容被拒绝: {detail}"),
            "CONTENT_BLOCKED",
            false,
        ));
    }
    if status == 429 || body.contains("Too Many Requests") {
        return Some(AdapterError::normalized(
            "触发限流/上游繁忙",
            "RATE_LIMITED",
            true,
        ));
    }
    if body.contains("recaptcha validation failed") {
        return Some(AdapterError::normalized(
            "触发人机验证",
            "CAPTCHA_REQUIRED",
            false,
        ));
    }
    if status >= 500 {
        let message = detail
            .map(|d| format!("上游服务器错误 ({status}): {d}"))
            .unwrap_or_else(|| format!("上游服务器错误，HTTP错误码: {status}"));
        return Some(AdapterError::normalized(message, "HTTP_ERROR", true));
    }
    if status >= 400 {
        let message = detail
            .map(|d| format!("请求被拒绝 ({status}): {d}"))
            .unwrap_or_else(|| format!("请求错误，HTTP错误码: {status}"));
        return Some(AdapterError::normalized(message, "HTTP_ERROR", false));
    }
    None
}

#[cfg(test)]
pub(crate) mod mock {
    use super::*;
    use std::{collections::VecDeque, sync::Mutex};

    #[derive(Default)]
    pub struct MockPage {
        pub results: Mutex<VecDeque<Value>>,
        pub calls: Mutex<Vec<Vec<Value>>>,
        pub events: Mutex<Vec<BrowserEvent>>,
        pub body: Mutex<String>,
    }

    impl MockPage {
        pub fn returning(results: impl IntoIterator<Item = Value>) -> Self {
            Self {
                results: Mutex::new(results.into_iter().collect()),
                ..Self::default()
            }
        }
    }

    impl PageClient for MockPage {
        fn call<'a>(&'a self, operations: Vec<Value>) -> ClientFuture<'a, Vec<Value>> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(operations.clone());
                let mut results = self.results.lock().unwrap();
                let output = operations
                    .iter()
                    .map(|_| results.pop_front().unwrap_or(Value::Null))
                    .collect();
                drop(results);
                for operation in operations {
                    if operation.get("op").and_then(Value::as_str) == Some("screenshot") {
                        if let Some(path) = operation.get("path").and_then(Value::as_str) {
                            std::fs::write(path, b"mock-png")
                                .map_err(|error| AdapterError::new(error.to_string()))?;
                        }
                    }
                }
                Ok(output)
            })
        }
        fn subscribe_responses<'a>(&'a self) -> ClientFuture<'a, u64> {
            Box::pin(async { Ok(0) })
        }
        fn poll_events<'a>(&'a self, _after_sequence: u64) -> ClientFuture<'a, Vec<BrowserEvent>> {
            Box::pin(async move { Ok(self.events.lock().unwrap().clone()) })
        }
        fn response_body<'a>(&'a self, _response_id: &'a str) -> ClientFuture<'a, Vec<u8>> {
            Box::pin(async move { Ok(self.body.lock().unwrap().clone().into_bytes()) })
        }
        fn response_wait_finished<'a>(
            &'a self,
            _response_id: &'a str,
            _timeout_ms: u64,
        ) -> ClientFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
    }
}
