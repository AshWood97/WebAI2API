//! Typed client for the generic browser runtime JSON-lines RPC.
//!
//! This protocol exposes browser primitives only. Site flows and decisions are
//! intentionally represented by Rust callers as batches of [`PageOperation`].

use crate::errors::BridgeError;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};

type ReplyTx = oneshot::Sender<Result<Value, BridgeError>>;

struct Inner {
    writer: mpsc::UnboundedSender<String>,
    pending: Mutex<HashMap<u64, ReplyTx>>,
    next_id: AtomicU64,
    events: tokio::sync::Mutex<mpsc::Receiver<Value>>,
}

/// Cloneable connection to `bridge/browser-runtime.mjs`.
#[derive(Clone)]
pub struct BrowserRpc {
    inner: Arc<Inner>,
}

impl BrowserRpc {
    /// Connect to the runtime's Unix socket.
    pub async fn connect(sock: PathBuf) -> Result<Self, BridgeError> {
        let stream = UnixStream::connect(&sock)
            .await
            .map_err(|source| BridgeError::Connect { path: sock, source })?;
        let (read_half, mut write_half) = stream.into_split();
        let (writer, mut outgoing) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            while let Some(line) = outgoing.recv().await {
                if write_half.write_all(line.as_bytes()).await.is_err()
                    || write_half.write_all(b"\n").await.is_err()
                    || write_half.flush().await.is_err()
                {
                    break;
                }
            }
        });
        // Event consumers may use event.poll instead of this live channel.
        // Bound the channel so an idle receiver cannot grow memory forever.
        let (event_tx, event_rx) = mpsc::channel(1024);
        let inner = Arc::new(Inner {
            writer,
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            events: tokio::sync::Mutex::new(event_rx),
        });
        let read_inner = Arc::clone(&inner);
        tokio::spawn(async move {
            let mut lines = BufReader::new(read_half).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(message) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if let Some(id) = message.get("id").and_then(Value::as_u64) {
                    if let Some(tx) = read_inner.pending.lock().unwrap().remove(&id) {
                        let result =
                            if let Some(error) = message.get("error").and_then(Value::as_str) {
                                Err(BridgeError::Remote(error.to_owned()))
                            } else {
                                Ok(message.get("result").cloned().unwrap_or(Value::Null))
                            };
                        let _ = tx.send(result);
                    }
                } else {
                    let _ = event_tx.try_send(message);
                }
            }
            for (_, tx) in read_inner.pending.lock().unwrap().drain() {
                let _ = tx.send(Err(BridgeError::Disconnected));
            }
        });
        Ok(Self { inner })
    }

    async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
        timeout: std::time::Duration,
    ) -> Result<T, BridgeError> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().unwrap().insert(id, tx);
        let frame = serde_json::to_string(&json!({"id": id, "method": method, "params": params}))?;
        if self.inner.writer.send(frame).is_err() {
            self.inner.pending.lock().unwrap().remove(&id);
            return Err(BridgeError::WriteClosed);
        }
        let value = match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result?,
            Ok(Err(_)) => return Err(BridgeError::ReplyChannelClosed),
            Err(_) => {
                self.inner.pending.lock().unwrap().remove(&id);
                return Err(BridgeError::Timeout {
                    method: method.to_owned(),
                });
            }
        };
        serde_json::from_value(value).map_err(BridgeError::Serialize)
    }

    async fn call_default<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
    ) -> Result<T, BridgeError> {
        self.call(method, params, std::time::Duration::from_secs(30))
            .await
    }

    /// Receive the next unsolicited runtime event, including the initial `ready` event.
    pub async fn next_event(&self) -> Option<Value> {
        self.inner.events.lock().await.recv().await
    }

    pub async fn preflight(&self) -> Result<PreflightResult, BridgeError> {
        self.call_default("preflight", json!({})).await
    }

    pub async fn preflight_with_config(
        &self,
        config: &Value,
    ) -> Result<PreflightResult, BridgeError> {
        self.call_default("preflight", json!({"config": config}))
            .await
    }

    pub async fn browser_start(
        &self,
        config: Value,
        options: Value,
    ) -> Result<BrowserStarted, BridgeError> {
        self.call(
            "browser.start",
            json!({"config": config, "options": options}),
            std::time::Duration::from_secs(600),
        )
        .await
    }

    pub async fn browser_close(&self, browser_id: &str) -> Result<OkResult, BridgeError> {
        self.call_default("browser.close", json!({"browserId": browser_id}))
            .await
    }

    pub async fn browser_restart(&self, browser_id: &str) -> Result<BrowserRestarted, BridgeError> {
        self.call(
            "browser.restart",
            json!({"browserId": browser_id}),
            std::time::Duration::from_secs(600),
        )
        .await
    }

    pub async fn browser_status(&self, browser_id: Option<&str>) -> Result<Value, BridgeError> {
        self.call_default("browser.status", json!({"browserId": browser_id}))
            .await
    }

    pub async fn page_create(
        &self,
        browser_id: &str,
        url: Option<&str>,
        timeout_ms: Option<u64>,
    ) -> Result<PageCreated, BridgeError> {
        self.call(
            "page.create",
            json!({"browserId": browser_id, "url": url, "timeoutMs": timeout_ms}),
            std::time::Duration::from_secs(120),
        )
        .await
    }

    pub async fn page_close(&self, page_id: &str) -> Result<OkResult, BridgeError> {
        self.call_default("page.close", json!({"pageId": page_id}))
            .await
    }

    /// Execute sequential page operations in one RPC. The runtime reports a
    /// failed operation index while preserving earlier operation results.
    pub async fn page_call(
        &self,
        page_id: &str,
        operations: Vec<PageOperation>,
        timeout: std::time::Duration,
    ) -> Result<PageCallResult, BridgeError> {
        self.call(
            "page.call",
            json!({"pageId": page_id, "operations": operations}),
            timeout,
        )
        .await
    }

    pub async fn event_subscribe(
        &self,
        browser_id: Option<&str>,
        page_id: Option<&str>,
        types: Vec<String>,
    ) -> Result<Subscription, BridgeError> {
        self.call_default(
            "event.subscribe",
            json!({"browserId": browser_id, "pageId": page_id, "types": types}),
        )
        .await
    }

    pub async fn event_unsubscribe(&self, subscription_id: &str) -> Result<OkResult, BridgeError> {
        self.call_default(
            "event.unsubscribe",
            json!({"subscriptionId": subscription_id}),
        )
        .await
    }

    pub async fn event_poll(
        &self,
        after_sequence: u64,
        limit: Option<usize>,
    ) -> Result<EventBatch, BridgeError> {
        self.call_default(
            "event.poll",
            json!({"afterSequence": after_sequence, "limit": limit}),
        )
        .await
    }

    pub async fn event_poll_page(
        &self,
        page_id: &str,
        after_sequence: u64,
        limit: Option<usize>,
    ) -> Result<EventBatch, BridgeError> {
        self.call_default(
            "event.poll",
            json!({"afterSequence": after_sequence, "limit": limit, "pageId": page_id}),
        )
        .await
    }

    pub async fn route_install(
        &self,
        page_id: &str,
        pattern: Option<&str>,
        timeout_ms: Option<u64>,
    ) -> Result<RouteInstalled, BridgeError> {
        self.call_default(
            "route.install",
            json!({"pageId": page_id, "pattern": pattern, "timeoutMs": timeout_ms}),
        )
        .await
    }

    pub async fn route_resolve(
        &self,
        route_token: &str,
        decision: RouteDecision,
    ) -> Result<OkResult, BridgeError> {
        let mut params = serde_json::to_value(decision)?;
        params["routeToken"] = json!(route_token);
        self.call_default("route.resolve", params).await
    }

    pub async fn route_remove(&self, route_id: &str) -> Result<OkResult, BridgeError> {
        self.call_default("route.remove", json!({"routeId": route_id}))
            .await
    }

    pub async fn response_body(
        &self,
        response_id: &str,
        path: Option<&str>,
    ) -> Result<ResponseBody, BridgeError> {
        self.call(
            "response.body",
            json!({"responseId": response_id, "path": path}),
            std::time::Duration::from_secs(120),
        )
        .await
    }

    pub async fn response_wait_finished(
        &self,
        response_id: &str,
        timeout_ms: u64,
    ) -> Result<OkResult, BridgeError> {
        self.call(
            "response.waitFinished",
            json!({"responseId": response_id, "timeoutMs": timeout_ms}),
            std::time::Duration::from_millis(timeout_ms.saturating_add(1_000)),
        )
        .await
    }

    pub async fn download_fetch(
        &self,
        page_id: &str,
        url: &str,
        path: &str,
        headers: Value,
        timeout_ms: Option<u64>,
    ) -> Result<DownloadResult, BridgeError> {
        self.call("download.fetch", json!({"pageId": page_id, "url": url, "path": path, "headers": headers, "timeoutMs": timeout_ms}), std::time::Duration::from_secs(300)).await
    }

    pub async fn cookies_get(
        &self,
        browser_id: &str,
        urls: Vec<String>,
    ) -> Result<CookiesResult, BridgeError> {
        self.call_default(
            "cookies.get",
            json!({"browserId": browser_id, "urls": urls}),
        )
        .await
    }

    pub async fn shutdown(&self) -> Result<OkResult, BridgeError> {
        self.call(
            "runtime.shutdown",
            json!({}),
            std::time::Duration::from_secs(30),
        )
        .await
    }
}

/// A generic operation object understood by `page.call`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageOperation {
    pub op: String,
    #[serde(flatten)]
    pub args: serde_json::Map<String, Value>,
}

impl PageOperation {
    pub fn new(op: impl Into<String>) -> Self {
        Self {
            op: op.into(),
            args: serde_json::Map::new(),
        }
    }
    pub fn with(mut self, key: impl Into<String>, value: impl Serialize) -> Self {
        if let Ok(value) = serde_json::to_value(value) {
            self.args.insert(key.into(), value);
        }
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteDecision {
    pub action: String,
    #[serde(flatten)]
    pub options: serde_json::Map<String, Value>,
}

impl RouteDecision {
    pub fn continue_request() -> Self {
        Self {
            action: "continue".into(),
            options: Default::default(),
        }
    }
    pub fn abort(error_code: Option<&str>) -> Self {
        let mut value = Self {
            action: "abort".into(),
            options: Default::default(),
        };
        if let Some(code) = error_code {
            value.options.insert("errorCode".into(), json!(code));
        }
        value
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreflightResult {
    pub ok: bool,
    #[serde(default)]
    pub skipped: bool,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserStarted {
    pub browser_id: String,
    pub engine: String,
    /// Engine metadata is a structured object whose fields vary by backend.
    pub runtime: Option<Value>,
    #[serde(default)]
    pub page_ids: Vec<String>,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserRestarted {
    #[serde(flatten)]
    pub started: BrowserStarted,
    pub previous_browser_id: String,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageCreated {
    pub page_id: String,
    pub generation: u64,
}
#[derive(Debug, Clone, Deserialize)]
pub struct PageCallResult {
    pub results: Vec<Value>,
    pub failed_index: Option<usize>,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Subscription {
    pub subscription_id: String,
    pub after_sequence: u64,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeEvent {
    pub event: String,
    pub sequence: Option<u64>,
    #[serde(flatten)]
    pub fields: serde_json::Map<String, Value>,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventBatch {
    pub events: Vec<RuntimeEvent>,
    pub dropped: u64,
    pub latest_sequence: u64,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteInstalled {
    pub route_id: String,
    pub pattern: String,
    pub timeout_ms: u64,
}
#[derive(Debug, Clone, Deserialize)]
pub struct ResponseBody {
    pub path: Option<String>,
    pub base64: Option<String>,
    pub bytes: u64,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadResult {
    pub path: String,
    pub bytes: u64,
    pub status: u16,
    pub headers: Value,
}
#[derive(Debug, Clone, Deserialize)]
pub struct CookiesResult {
    pub cookies: Vec<Value>,
}
#[derive(Debug, Clone, Deserialize)]
pub struct OkResult {
    pub ok: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixListener;

    fn socket_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "webai2api-rpc-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[tokio::test]
    async fn typed_page_batch_and_unsolicited_events_round_trip() {
        let path = socket_path();
        let listener = UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            write.write_all(b"{\"event\":\"ready\"}\n").await.unwrap();
            let mut lines = BufReader::new(read).lines();
            let request: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(request["method"], "page.call");
            assert_eq!(request["params"]["operations"][0]["op"], "goto");
            let id = request["id"].as_u64().unwrap();
            write.write_all(format!("{{\"id\":{id},\"result\":{{\"results\":[\"https://example.test\"],\"failedIndex\":null}}}}\n").as_bytes()).await.unwrap();
        });
        let rpc = BrowserRpc::connect(path.clone()).await.unwrap();
        assert_eq!(rpc.next_event().await.unwrap()["event"], "ready");
        let result = rpc
            .page_call(
                "p",
                vec![PageOperation::new("goto").with("url", "https://example.test")],
                std::time::Duration::from_secs(2),
            )
            .await
            .unwrap();
        assert_eq!(result.results[0], "https://example.test");
        assert_eq!(result.failed_index, None);
        server.await.unwrap();
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn remote_errors_are_preserved() {
        let path = socket_path();
        let listener = UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let request: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            write
                .write_all(
                    format!(
                        "{{\"id\":{},\"error\":\"unknown browserId\"}}\n",
                        request["id"]
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        let rpc = BrowserRpc::connect(path.clone()).await.unwrap();
        let error = rpc.browser_close("bad-id").await.unwrap_err();
        assert!(matches!(error, BridgeError::Remote(message) if message == "unknown browserId"));
        server.await.unwrap();
        let _ = std::fs::remove_file(path);
    }
}
