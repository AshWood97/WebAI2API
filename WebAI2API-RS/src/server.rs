//! HTTP 服务，路由与行为对应原 `src/server/api/`。
//! 路由顺序：免鉴权诊断端点 → 静态 WebUI → Bearer 鉴权 → /admin → /v1。

use crate::config::{self, ConfigError};
use crate::errors::error_detail;
use crate::history::{self, ListFilter, RecordUpdate};
use crate::parse::{self, ParseError};
use crate::queue::{Queue, Task};
use crate::respond::{self, anthropic_error_event};
use crate::runtime::BackendRuntime;
use crate::stats;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use axum::Router;
use base64::Engine;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// 重启回调类型（main.rs 注入，把重启请求发回主循环）。
pub type RestartCallback = Mutex<Option<Box<dyn Fn(Vec<String>) + Send + Sync>>>;

pub struct AppState {
    pub config: Value,
    pub bridge: BackendRuntime,
    pub queue: Queue,
    pub webui_dir: PathBuf,
    pub data_dir: PathBuf,
    pub temp_dir: PathBuf,
    pub started_at: Instant,
    pub safe_mode: Mutex<Option<String>>,
    pub login_mode: bool,
    pub login_worker: Option<String>,
    /// 重启回调：参数为新的启动参数（如 -login）。
    pub restart: RestartCallback,
    pub vnc: Mutex<VncInfo>,
    /// 引擎桥 init 返回的 worker 运行时快照（/v1/runtime/status、/v1/auth/status 用）。
    pub workers: Mutex<Value>,
    /// 浏览器是否被用户手动关闭（需 POST /admin/browser/restart 恢复）。
    pub browser_stopped: Mutex<bool>,
    /// config.yaml 真实路径（src-root/data/config.yaml，可能与 data_dir 不同）。
    pub config_path: PathBuf,
    /// 原仓库根目录（含 camoufox/、node_modules/）。
    pub src_root: PathBuf,
    /// 停机信号：/admin/stop 与 IPC STOP 发送，主循环统一清理后退出。
    pub stop: tokio::sync::mpsc::UnboundedSender<()>,
    /// 配置写回互斥，防止并发保存互相覆盖。
    pub config_save: Mutex<()>,
    /// 模型表缓存（桥 init 后基本静态）：None = 未加载。桥崩溃重启后失效需重新拉取。
    pub models_cache: Mutex<Option<Value>>,
}

#[derive(Clone, Default)]
pub struct VncInfo {
    pub enabled: bool,
    pub port: u16,
    pub display: String,
    pub xvfb_mode: bool,
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", any(dispatch))
        .route("/{*path}", any(dispatch))
        .route("/admin/vnc", get(crate::webui::vnc_ws))
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
        .with_state(state)
}

// ==================== 模型表缓存 ====================

impl AppState {
    /// 模型表缓存读取：命中直接返回；未命中走一次桥 IPC 并回填。
    /// 桥不可用时返回空表（原版语义：错误由各端点自行处理）。
    pub async fn models_table(&self) -> Value {
        if let Some(cached) = self.models_cache.lock().unwrap().clone() {
            return cached;
        }
        let table = self
            .bridge
            .get_models()
            .await
            .unwrap_or_else(|_| json!({"data": []}));
        *self.models_cache.lock().unwrap() = Some(table.clone());
        table
    }

    /// 失效缓存（桥重启、/admin/browser/restart 之后适配器集合可能变化）。
    pub fn invalidate_models_cache(&self) {
        *self.models_cache.lock().unwrap() = None;
    }
}

/// 聊天准入需要的模型元信息：(model_type, image_policy, model_known)。
/// 空 modelId 时保持原版默认值；否则一次聚合 IPC 完成三个查询。
async fn model_meta(state: &AppState, model_id: &str) -> Result<(String, String, bool), String> {
    if model_id.is_empty() {
        return Ok(("image".to_string(), "optional".to_string(), true));
    }
    let info = state
        .bridge
        .model_info(model_id)
        .await
        .map_err(|e| e.to_string())?;
    let table = info
        .get("models")
        .cloned()
        .unwrap_or_else(|| json!({"data": []}));
    *state.models_cache.lock().unwrap() = Some(table.clone());
    let known = table
        .get("data")
        .and_then(Value::as_array)
        .is_some_and(|d| {
            d.iter()
                .any(|m| m.get("id").and_then(Value::as_str) == Some(model_id))
        });
    let model_type = info
        .get("modelType")
        .and_then(Value::as_str)
        .unwrap_or("image")
        .to_string();
    let image_policy = info
        .get("imagePolicy")
        .and_then(Value::as_str)
        .unwrap_or("optional")
        .to_string();
    Ok((model_type, image_policy, known))
}

// ==================== 总调度 ====================

async fn dispatch(State(state): State<Arc<AppState>>, req: axum::extract::Request) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();
    let headers = req.headers().clone();

    // 免鉴权诊断端点
    if method == axum::http::Method::GET {
        match path.as_str() {
            "/health" | "/healthz" => return json_response(200, health_body(&state)),
            "/ready" | "/readyz" => return ready_response(&state),
            "/openapi.json" | "/openapi" => return openapi_response(&state).await,
            "/docs" | "/docs/" => return docs_response(),
            _ => {}
        }
        // 静态 WebUI：非 /v1 非 /admin 的 GET（鉴权之前，资产公开）
        if !path.starts_with("/v1") && !path.starts_with("/admin") {
            if let Some(resp) = crate::webui::serve_static(&state, &path).await {
                return resp;
            }
        }
    }

    // 鉴权：auth 为空则跳过；否则 Authorization 必须精确等于 `Bearer <token>`
    let auth = config::auth_of(&state.config);
    if !auth.is_empty() {
        let expected = format!("Bearer {auth}");
        if !token_eq(
            headers
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .unwrap_or(""),
            &expected,
        ) {
            return api_error("UNAUTHORIZED", None, None, false);
        }
    }

    if path.starts_with("/admin") {
        let body = read_body(req).await;
        return admin(
            &state,
            method.as_str(),
            &path["/admin".len().min(path.len())..],
            &query,
            &body,
        )
        .await;
    }

    if let Some(v1_path) = path.strip_prefix("/v1") {
        let is_anthropic = v1_path.starts_with("/messages");
        // 安全模式与登录模式：诊断端点放行，其余 503
        let diagnostic = method == axum::http::Method::GET
            && (v1_path.starts_with("/models")
                || v1_path.starts_with("/runtime/status")
                || v1_path.starts_with("/auth/status")
                || v1_path.starts_with("/providers")
                || v1_path.starts_with("/openapi"));
        if !diagnostic {
            if let Some(reason) = state.safe_mode.lock().unwrap().clone() {
                return if is_anthropic {
                    anthropic_error(503, &format!("Service in safe mode: {reason}"), false)
                } else {
                    json_response(
                        503,
                        json!({"error": {
                        "message": format!("服务运行在安全模式，OpenAI API 不可用。原因: {reason}"),
                        "type": "service_unavailable", "code": "SERVICE_UNAVAILABLE" }}),
                    )
                };
            }
            if state.login_mode {
                return if is_anthropic {
                    anthropic_error(
                        503,
                        "Service in login mode, generation API unavailable",
                        false,
                    )
                } else {
                    json_response(
                        503,
                        json!({"error": {
                        "message": "服务运行在登录模式，OpenAI API 不可用",
                        "type": "service_unavailable", "code": "SERVICE_UNAVAILABLE" }}),
                    )
                };
            }
        }
        if is_anthropic {
            return anthropic(&state, method.as_str(), req).await;
        }
        return openai(&state, method.as_str(), v1_path, &query, req).await;
    }

    json_response(
        404,
        json!({"error": {"message": "Not Found", "code": "NOT_FOUND"}}),
    )
}

// ==================== OpenAI 路由 ====================

async fn openai(
    state: &Arc<AppState>,
    method: &str,
    v1_path: &str,
    query: &str,
    req: axum::extract::Request,
) -> Response {
    if method == "GET" && v1_path == "/models" {
        return match state.bridge.get_models().await {
            Ok(models) => json_response(200, models),
            Err(e) => api_error("INTERNAL_ERROR", Some(&e.to_string()), None, false),
        };
    }
    if method == "GET" && (v1_path == "/openapi.json" || v1_path == "/openapi") {
        return openapi_response(state).await;
    }
    if method == "GET" && v1_path == "/runtime/status" {
        return runtime_status(state).await;
    }
    if method == "GET" && v1_path == "/auth/status" {
        return auth_status(state).await;
    }
    if method == "GET" && v1_path == "/providers" {
        return providers(state).await;
    }
    if method == "GET" && v1_path == "/cookies" {
        return cookies(state, query).await;
    }
    // /v1/chat/completions 与 /v1/stateless/chat/completions 用前缀匹配（保留原行为）
    if method == "POST"
        && (v1_path.starts_with("/chat/completions")
            || v1_path.starts_with("/stateless/chat/completions"))
    {
        let stateless = v1_path.starts_with("/stateless/");
        return chat_completions(state, req, stateless).await;
    }
    if method == "GET" && v1_path == "/stateless/models" {
        return match state.bridge.get_models().await {
            Ok(mut models) => {
                if let Some(data) = models.get_mut("data").and_then(Value::as_array_mut) {
                    data.retain(|m| m.get("type").and_then(Value::as_str) == Some("text"));
                }
                json_response(200, models)
            }
            Err(e) => api_error("INTERNAL_ERROR", Some(&e.to_string()), None, false),
        };
    }
    json_response(
        404,
        json!({"error": {
        "message": format!("Unknown path: /v1{v1_path}"),
        "type": "invalid_request_error", "code": "NOT_FOUND" }}),
    )
}

async fn chat_completions(
    state: &Arc<AppState>,
    req: axum::extract::Request,
    stateless: bool,
) -> Response {
    let raw = read_body(req).await;
    let mut body: Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        // 保留原行为：JSON 解析失败走 500 INTERNAL_ERROR
        Err(_) => return api_error("INTERNAL_ERROR", None, None, false),
    };
    if stateless {
        body["stateless"] = json!(true);
        if body.get("conversation_id").is_some() {
            return api_error(
                "INVALID_REQUEST_BODY",
                Some("stateless endpoint rejects conversation_id; client owns history"),
                None,
                false,
            );
        }
    }
    let is_streaming = body.get("stream").and_then(Value::as_bool).unwrap_or(false);

    let model_id = body.get("model").and_then(Value::as_str).unwrap_or("");
    let (model_type, image_policy, model_known) = match model_meta(state, model_id).await {
        Ok(v) => v,
        Err(e) => return api_error("INTERNAL_ERROR", Some(&e), None, is_streaming),
    };

    let parsed = parse::parse_request(
        &body,
        &model_type,
        &image_policy,
        config::image_limit(&state.config),
        &state.temp_dir,
        "pool",
        model_known,
    )
    .await;
    let parsed = match parsed {
        Ok(p) => p,
        Err(ParseError { code, message }) => {
            return api_error(
                code,
                if message.is_empty() {
                    None
                } else {
                    Some(&message)
                },
                None,
                is_streaming,
            )
        }
    };

    let reasoning = body
        .get("reasoning")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || stateless;

    // 限流只对非流式生效。try_reserve 占用名额，堵住"检查通过到入队"之间的并发空隙。
    // 保留原瑕疵：消息里的上限显示为 undefined（queue.js 未定义 maxQueueSize）
    if !is_streaming && !state.queue.try_reserve() {
        // 准入失败不入队，清理 parse 阶段已落盘的临时图片
        for p in &parsed.image_paths {
            std::fs::remove_file(p).ok();
        }
        let msg = format!(
            "服务器繁忙（队列: {}/undefined）。请使用流式模式 (stream: true) 或稍后重试。",
            state.queue.status_total()
        );
        return api_error("SERVER_BUSY", Some(&msg), None, false);
    }

    let id = short_id();
    let (sink_tx, mut sink_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();

    state.queue.enqueue(Task {
        id,
        prompt: parsed.prompt,
        image_paths: parsed.image_paths,
        model_id: parsed.model_id,
        model_name: parsed.model_name,
        is_streaming,
        reasoning,
        sink: sink_tx,
        reply: if is_streaming { None } else { Some(reply_tx) },
        enqueued_at: Instant::now(),
    });

    if is_streaming {
        let stream = async_stream::stream! {
            while let Some(chunk) = sink_rx.recv().await {
                yield Ok::<Vec<u8>, std::io::Error>(chunk.into_bytes());
            }
        };
        return Response::builder()
            .status(200)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .header(header::CACHE_CONTROL, "no-cache")
            .header(header::CONNECTION, "keep-alive")
            .body(Body::from_stream(stream))
            .unwrap();
    }

    match reply_rx.await {
        Ok((status, body)) => json_response(status, body),
        Err(_) => api_error("INTERNAL_ERROR", Some("队列任务丢失"), None, false),
    }
}

// ==================== Anthropic 路由 ====================

async fn anthropic(state: &Arc<AppState>, method: &str, req: axum::extract::Request) -> Response {
    if method != "POST" {
        return anthropic_error(
            400,
            &format!("Method {method} not allowed. Use POST /v1/messages"),
            false,
        );
    }
    let raw = read_body(req).await;
    let body: Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(e) => return anthropic_error(400, &format!("Invalid JSON body: {e}"), false),
    };
    if state.safe_mode.lock().unwrap().is_some() {
        let reason = state.safe_mode.lock().unwrap().clone().unwrap_or_default();
        return anthropic_error(503, &format!("Service in safe mode: {reason}"), false);
    }
    let is_streaming = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    if !is_streaming && !state.queue.try_reserve() {
        let msg = format!(
            "Server busy (queue: {}). Use stream: true or retry later.",
            state.queue.status_total()
        );
        return anthropic_error(429, &msg, false);
    }

    // Anthropic → 内部消息：system 前置、图片块转 data URI、tool 块转文本，强制 stateless+reasoning
    let mut messages: Vec<Value> = Vec::new();
    if let Some(system) = body.get("system") {
        let text = match system {
            Value::String(s) => s.clone(),
            Value::Array(parts) => parts
                .iter()
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        if !text.is_empty() {
            messages.push(json!({"role": "system", "content": text}));
        }
    }
    if let Some(arr) = body.get("messages").and_then(Value::as_array) {
        for m in arr {
            let role = m.get("role").and_then(Value::as_str).unwrap_or("user");
            let content = match m.get("content") {
                Some(Value::String(s)) => json!(s),
                Some(Value::Array(parts)) => {
                    let mut text = String::new();
                    let mut images = Vec::new();
                    for p in parts {
                        match p.get("type").and_then(Value::as_str) {
                            Some("text") => {
                                text.push_str(p.get("text").and_then(Value::as_str).unwrap_or(""))
                            }
                            Some("image") => {
                                let media = p
                                    .pointer("/source/media_type")
                                    .and_then(Value::as_str)
                                    .unwrap_or("image/png");
                                let data = p
                                    .pointer("/source/data")
                                    .and_then(Value::as_str)
                                    .unwrap_or("");
                                images.push(json!({"type": "image_url", "image_url": {"url": format!("data:{media};base64,{data}")}}));
                            }
                            Some("tool_result") => {
                                let c = p
                                    .get("content")
                                    .map(|v| {
                                        if v.is_string() {
                                            v.as_str().unwrap().to_string()
                                        } else {
                                            v.to_string()
                                        }
                                    })
                                    .unwrap_or_default();
                                text.push_str(&format!("[tool_result] {c}"));
                            }
                            Some("tool_use") => {
                                let name = p.get("name").and_then(Value::as_str).unwrap_or("");
                                text.push_str(&format!(
                                    "[tool_use:{name}] {}",
                                    p.get("input").unwrap_or(&Value::Null)
                                ));
                            }
                            _ => {}
                        }
                    }
                    if images.is_empty() {
                        json!(text)
                    } else {
                        let mut parts = vec![json!({"type": "text", "text": text})];
                        parts.extend(images);
                        json!(parts)
                    }
                }
                _ => json!(""),
            };
            messages.push(json!({"role": role, "content": content}));
        }
    }
    let internal = json!({
        "model": body.get("model").and_then(Value::as_str).unwrap_or(""),
        "messages": messages,
        "stream": is_streaming,
        "stateless": true,
        "reasoning": true,
    });

    let model_hint = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let model_id = model_hint.as_str();
    let (model_type, image_policy, model_known) = match model_meta(state, model_id).await {
        Ok(v) => v,
        Err(_) => {
            if !is_streaming {
                state.queue.cancel_reservation();
            }
            return anthropic_error(500, "bridge error", false);
        }
    };
    let parsed = match parse::parse_request(
        &internal,
        &model_type,
        &image_policy,
        config::image_limit(&state.config),
        &state.temp_dir,
        "pool",
        model_known,
    )
    .await
    {
        Ok(p) => p,
        Err(ParseError { code, message }) => {
            if !is_streaming {
                state.queue.cancel_reservation();
            }
            let d = error_detail(code);
            let msg = if message.is_empty() {
                d.message.to_string()
            } else {
                message
            };
            return anthropic_error(d.status, &msg, false);
        }
    };

    let id = short_id();
    let (sink_tx, mut sink_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let model_name = parsed.model_name.clone();
    state.queue.enqueue(Task {
        id,
        prompt: parsed.prompt,
        image_paths: parsed.image_paths,
        model_id: parsed.model_id,
        model_name: model_name.clone(),
        is_streaming,
        reasoning: true,
        sink: sink_tx,
        reply: if is_streaming { None } else { Some(reply_tx) },
        enqueued_at: Instant::now(),
    });

    let model_for_output = if model_name.is_empty() {
        model_hint
    } else {
        model_name
    };

    if is_streaming {
        // 缓冲 OpenAI 伪流式输出，结束时一次性回放 Anthropic 事件序列
        let stream = async_stream::stream! {
            let mut content = String::new();
            let mut reasoning = String::new();
            let mut error_payload: Option<Value> = None;
            while let Some(chunk) = sink_rx.recv().await {
                for line in chunk.lines() {
                    if let Some(data) = line.strip_prefix("data: ") {
                        if data == "[DONE]" { continue; }
                        if let Ok(v) = serde_json::from_str::<Value>(data) {
                            if let Some(c) = v.pointer("/choices/0/delta/content").and_then(Value::as_str) { content.push_str(c); }
                            if let Some(r) = v.pointer("/choices/0/delta/reasoning_content").and_then(Value::as_str) { reasoning.push_str(r); }
                            if v.get("error").is_some() { error_payload = Some(v); }
                        }
                    }
                }
            }
            if let Some(err) = error_payload {
                let payload = json!({"type": "error", "error": {"type": "api_error", "message": err["error"]["message"]}});
                yield Ok::<Vec<u8>, std::io::Error>(anthropic_error_event(&payload).into_bytes());
            } else {
                let events = respond::anthropic_events(&content, Some(&model_for_output),
                    if reasoning.is_empty() { None } else { Some(reasoning.as_str()) });
                yield Ok(events.into_bytes());
            }
        };
        return Response::builder()
            .status(200)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .header(header::CACHE_CONTROL, "no-cache")
            .header(header::CONNECTION, "keep-alive")
            .header("x-accel-buffering", "no")
            .body(Body::from_stream(stream))
            .unwrap();
    }

    match reply_rx.await {
        Ok((status, body)) => {
            if status != 200 {
                let msg = body["error"]["message"]
                    .as_str()
                    .unwrap_or("generation failed");
                return anthropic_error(502, msg, false);
            }
            let content = body
                .pointer("/choices/0/message/content")
                .and_then(Value::as_str)
                .unwrap_or("");
            let reasoning = body
                .pointer("/choices/0/message/reasoning_content")
                .and_then(Value::as_str);
            json_response(
                200,
                respond::anthropic_message(content, Some(&model_for_output), reasoning, 0),
            )
        }
        Err(_) => anthropic_error(500, "队列任务丢失", false),
    }
}

fn anthropic_error(status: u16, message: &str, streaming: bool) -> Response {
    let body = json!({"type": "error", "error": {"type": crate::errors::anthropic_error_type(status), "message": message}});
    if streaming {
        (
            [(header::CONTENT_TYPE, "text/event-stream")],
            anthropic_error_event(&body),
        )
            .into_response()
    } else {
        json_response(status, body)
    }
}

// ==================== 诊断端点 ====================

fn health_body(state: &AppState) -> Value {
    json!({
        "status": "ok",
        "service": "webai2api",
        "version": VERSION,
        "uptime": state.started_at.elapsed().as_secs(),
        "timestamp": chrono::Utc::now().to_rfc3339(),
    })
}

fn ready_response(state: &AppState) -> Response {
    let safe = state.safe_mode.lock().unwrap().clone();
    let pool_ready = safe.is_none() && !state.login_mode;
    let ready = pool_ready;
    json_response(
        if ready { 200 } else { 503 },
        json!({
            "ready": ready,
            "service": "webai2api",
            "safeMode": safe.is_some(),
            "safeModeReason": safe,
            "loginMode": state.login_mode,
            "poolReady": pool_ready,
            "timestamp": chrono::Utc::now().to_rfc3339(),
        }),
    )
}

async fn openapi_response(state: &AppState) -> Response {
    let ids: Vec<String> = state
        .models_table()
        .await
        .get("data")
        .and_then(Value::as_array)
        .map(|d| {
            d.iter()
                .filter_map(|x| x.get("id").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    json_response(200, crate::openapi::build_schema(&ids, VERSION))
}

fn docs_response() -> Response {
    let html = include_str!("docs.html");
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html.replace("{{version}}", VERSION),
    )
        .into_response()
}

async fn runtime_status(state: &AppState) -> Response {
    let models = state.models_table().await;
    let data = models
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let (text, image) = (
        data.iter().filter(|m| m["type"] == "text").count(),
        data.iter().filter(|m| m["type"] != "text").count(),
    );
    let queue = state.queue.detailed_status();
    let (success, failed) = stats::today_stats().await;
    let safe = state.safe_mode.lock().unwrap().clone();
    let pool_ready = safe.is_none() && !state.login_mode;
    let status = if safe.is_some() {
        "safe_mode"
    } else if state.login_mode {
        "login_mode"
    } else if pool_ready {
        "ready"
    } else {
        "starting"
    };
    let sys = crate::metrics::system_status(state);
    let engines = referenced_engine_names(&state.config);
    let typed = crate::typed::Config::from_value(&state.config);
    let worker_snapshot = state
        .bridge
        .worker_snapshot()
        .unwrap_or_else(|| state.workers.lock().unwrap().clone());
    let workers_arr = worker_snapshot
        .get("workers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let browser_stopped = workers_arr
        .iter()
        .any(|worker| worker.get("stopped") == Some(&json!(true)))
        || *state.browser_stopped.lock().unwrap();
    // 与 Node 原版对齐：runtime 字段名改为 instance/runtime/stopped，去掉桥内部字段
    let workers = workers_arr
        .iter()
        .map(|w| {
            json!({
                "name": w["name"],
                "instance": w["instance"],
                "engine": w["engine"],
                "userDataDir": w["userDataDir"],
                "stopped": w["stopped"],
                "runtime": w["runtime"]
            })
        })
        .collect::<Vec<_>>();
    json_response(
        200,
        json!({
            "service": "webai2api",
            "backend": "pool",
            "version": VERSION,
            "uptime": state.started_at.elapsed().as_secs(),
            "status": status,
            "safeMode": { "enabled": safe.is_some(), "reason": safe },
            "loginMode": state.login_mode,
            "pool": {
                "ready": pool_ready,
                "workerCount": workers.len(),
                "strategy": typed.pool_strategy()
            },
            "queue": {
                "processing": queue["processing"], "waiting": queue["waiting"], "total": queue["total"],
                "maxConcurrent": config::max_concurrent(&state.config),
                "processingTasks": queue["processingTasks"], "waitingTasks": queue["waitingTasks"]
            },
            "models": { "count": data.len(), "text": text, "image": image },
            "stats": { "todaySuccess": success, "todayFailed": failed },
            "system": {
                "status": sys["status"], "cpuUsage": sys["cpuUsage"],
                "memoryUsage": sys["memoryUsage"], "systemVersion": sys["systemVersion"]
            },
            "camoufox": read_camoufox_version(),
            "browser": {
                "defaultEngine": typed.browser_engine(),
                "engines": engines,
                "clearcoteSdk": if typed.browser_engine() == "clearcote" {
                    read_clearcote_sdk_version()
                } else { json!(null) },
                "userStopped": browser_stopped,
                "workers": workers
            },
            "keepaliveMode": config::keepalive_mode(&state.config),
            "timestamp": chrono::Utc::now().to_rfc3339(),
        }),
    )
}

async fn auth_status(state: &AppState) -> Response {
    // 与 Node 的 getPoolContext() 一样只读缓存快照，不重复触发 init
    let snapshot = state
        .bridge
        .worker_snapshot()
        .unwrap_or_else(|| state.workers.lock().unwrap().clone());
    let pool_ready = state.safe_mode.lock().unwrap().is_none();
    let mut workers: Vec<Value> = snapshot
        .get("workers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    // 只保留 Node 原版暴露的字段
    for w in workers.iter_mut() {
        if let Some(obj) = w.as_object_mut() {
            obj.retain(|k, _| {
                matches!(
                    k.as_str(),
                    "name"
                        | "instance"
                        | "type"
                        | "adapter"
                        | "busy"
                        | "busyCount"
                        | "pageReady"
                        | "authReady"
                        | "mergeTypes"
                )
            });
        }
    }
    // 配置了但未运行的实例
    let typed = crate::typed::Config::from_value(&state.config);
    let configured: Vec<Value> = typed.backend.pool.instances.clone().unwrap_or_default();
    let running_names: Vec<String> = workers
        .iter()
        .filter_map(|w| w.get("name").and_then(Value::as_str).map(String::from))
        .collect();
    for inst in &configured {
        let inst_name = inst.get("name").and_then(Value::as_str).unwrap_or_default();
        if let Some(list) = inst.get("workers").and_then(Value::as_array) {
            for w in list {
                let name = w.get("name").and_then(Value::as_str).unwrap_or_default();
                if running_names.iter().any(|n| n == name) {
                    continue;
                }
                workers.push(json!({
                    "name": name,
                    "instance": inst_name,
                    "type": w.get("type"),
                    "authReady": false,
                    "pageReady": false,
                    "busy": false,
                    "status": "not_running"
                }));
            }
        }
    }
    let total = workers.len();
    let ready_count = workers
        .iter()
        .filter(|w| w.get("authReady") == Some(&json!(true)))
        .count();
    json_response(
        200,
        json!({
            "object": "auth.status",
            "backend": "pool",
            "poolReady": pool_ready,
            "loginMode": state.login_mode,
            "readyCount": ready_count,
            "totalCount": total,
            "overall": if pool_ready && ready_count > 0 { "authenticated" } else { "unknown" },
            "workers": workers,
            "note": "authReady 为粗粒度信号：浏览器实例已初始化即视为可用。精确登录态需通过 /v1/cookies 或 WebUI 人工确认。",
            "timestamp": chrono::Utc::now().to_rfc3339(),
        }),
    )
}

async fn providers(state: &AppState) -> Response {
    let models = state.models_table().await;
    let data = models
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let list = provider_list(&data, &state.config);
    json_response(
        200,
        json!({
            "object": "providers.list",
            "backend": "pool",
            "count": list.len(),
            "data": list,
            "timestamp": chrono::Utc::now().to_rfc3339(),
        }),
    )
}

async fn cookies(state: &AppState, query: &str) -> Response {
    let q: Vec<(String, String)> = url_query(query);
    let name = q.iter().find(|(k, _)| k == "name").map(|(_, v)| v.clone());
    let domain = q
        .iter()
        .find(|(k, _)| k == "domain")
        .map(|(_, v)| v.clone());
    if state.safe_mode.lock().unwrap().is_some() {
        return api_error("BROWSER_NOT_INITIALIZED", None, None, false);
    }
    match state
        .bridge
        .get_cookies(name.as_deref(), domain.as_deref())
        .await
    {
        Ok(v) => json_response(200, v),
        Err(e) if e.to_string().contains("浏览器实例不存在") => json_response(
            404,
            json!({"error":{"message":e.to_string(),"type":"invalid_request_error","code":"NOT_FOUND"}}),
        ),
        Err(e)
            if e.to_string().contains("Worker 不存在")
                || e.to_string().contains("Worker not found") =>
        {
            api_error("INVALID_MODEL", Some(&e.to_string()), None, false)
        }
        Err(e) => api_error("INTERNAL_ERROR", Some(&e.to_string()), None, false),
    }
}

// ==================== Admin 路由 ====================

async fn admin(
    state: &Arc<AppState>,
    method: &str,
    path: &str,
    query: &str,
    body: &[u8],
) -> Response {
    let json_body = |b: &[u8]| serde_json::from_slice::<Value>(b).unwrap_or(json!({}));
    match (method, path) {
        ("GET", "/status") => {
            let safe = state.safe_mode.lock().unwrap().clone();
            let mut body = crate::metrics::system_status(state);
            body["safeMode"] = json!({ "enabled": safe.is_some(), "reason": safe });
            json_response(200, body)
        }
        ("POST", "/restart") => {
            let b = json_body(body);
            let login = b.get("loginMode").and_then(Value::as_bool).unwrap_or(false);
            let worker = b.get("workerName").and_then(Value::as_str).unwrap_or("");
            let mode_text = if login {
                if worker.is_empty() {
                    "登录模式".to_string()
                } else {
                    format!("登录模式 ({worker})")
                }
            } else {
                "普通模式".to_string()
            };
            let args = if login {
                vec![if worker.is_empty() {
                    "-login".to_string()
                } else {
                    format!("-login={worker}")
                }]
            } else {
                vec![]
            };
            if let Some(f) = state.restart.lock().unwrap().as_ref() {
                f(args);
            }
            json_response(
                200,
                json!({"success": true, "message": format!("服务正在以{mode_text}重启...")}),
            )
        }
        // POST /admin/browser/restart：恢复被用户手动关闭的浏览器（原版同款语义）
        ("POST", "/browser/restart") => match state.bridge.restart_browser().await {
            Ok(v) => {
                let stopped = v
                    .get("browserStopped")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                *state.browser_stopped.lock().unwrap() = stopped;
                if let Some(ws) = v.get("workers").and_then(Value::as_array) {
                    *state.workers.lock().unwrap() = json!({ "workers": ws });
                }
                // 重启后适配器集合可能变化，模型表缓存需重新拉取
                state.invalidate_models_cache();
                match v.get("result").cloned().unwrap_or(Value::Null) {
                    r if r.get("success") == Some(&json!(true)) => json_response(200, r),
                    r if !r.is_null() => json_response(409, r),
                    _ => api_error(
                        "INTERNAL_ERROR",
                        Some("工作池未初始化（可能处于安全模式），请先通过 WebUI 重启服务"),
                        None,
                        false,
                    ),
                }
            }
            Err(e) => api_error("INTERNAL_ERROR", Some(&e.to_string()), None, false),
        },
        ("POST", "/stop") => {
            // 先应答再停机：主循环收到信号后会执行浏览器清理（原版同款语义）
            let _ = state.stop.send(());
            json_response(200, json!({"success": true, "message": "服务正在停止..."}))
        }
        ("GET", "/vnc/status") => {
            let v = state.vnc.lock().unwrap();
            json_response(
                200,
                json!({"enabled": v.enabled, "port": v.port, "display": v.display, "xvfbMode": v.xvfb_mode}),
            )
        }
        ("POST", "/cache/clear") => {
            let cleaned = crate::webui::clear_dir(&state.temp_dir);
            json_response(200, json!({"success": true, "cleaned": cleaned}))
        }
        ("GET", "/logs") => {
            let lines = url_query(query)
                .iter()
                .find(|(k, _)| k == "lines")
                .and_then(|(_, v)| v.parse::<usize>().ok())
                .unwrap_or(200);
            let (tail, total, file) = crate::logfmt::read_tail(lines);
            json_response(200, json!({"logs": tail, "total": total, "file": file}))
        }
        ("DELETE", "/logs") => {
            crate::logfmt::clear();
            json_response(200, json!({"success": true, "message": "日志已清除"}))
        }
        ("GET", "/data-folders") => {
            // dir_size 递归扫描可能很慢，整体计算放阻塞线程池
            let data_dir = crate::webui::profile_data_dir(state);
            let config = state.config.clone();
            let folders =
                tokio::task::spawn_blocking(move || crate::webui::data_folders(&data_dir, &config))
                    .await
                    .unwrap_or_else(|_| Value::Array(Vec::new()));
            json_response(200, folders)
        }
        ("POST", "/data-folders/delete") => {
            let b = json_body(body);
            let names: Vec<String> = b
                .get("names")
                .or_else(|| b.get("folders"))
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            json_response_status(crate::webui::delete_data_folders(
                &crate::webui::profile_data_dir(state),
                &names,
            ))
        }
        ("GET", "/config/server") => json_response(200, server_config_view(&state.config)),
        ("GET", "/config/browser") => json_response(200, browser_config_view(&state.config)),
        ("GET", "/config/instances") | ("GET", "/config/workers") => {
            json_response(200, instances_config_view(&state.config))
        }
        ("GET", "/config/adapters") => {
            let typed = crate::typed::Config::from_value(&state.config);
            json_response(200, Value::Object(typed.backend.adapter))
        }
        ("GET", "/config/pool") => json_response(200, pool_config_view(&state.config)),
        ("POST", p) if p.starts_with("/config/") => save_config(state, p, body).await,
        ("GET", "/adapters") => match state.bridge.list_adapters().await {
            Ok(v) => json_response(200, adapters_meta(&v, &state.config)),
            Err(e) => api_error("INTERNAL_ERROR", Some(&e.to_string()), None, false),
        },
        ("GET", "/stats") => {
            let (success, failed) = stats::today_stats().await;
            let typed = crate::typed::Config::from_value(&state.config);
            let instances = typed
                .backend
                .pool
                .instances
                .as_ref()
                .map(|a| a.len())
                .unwrap_or(0);
            let workers = typed
                .backend
                .pool
                .workers
                .as_ref()
                .map(|a| a.len())
                .unwrap_or(0);
            json_response(
                200,
                json!({"instances": instances, "workers": workers, "success": success, "failed": failed}),
            )
        }
        ("GET", "/stats/range") => stats_range(query, false).await,
        ("DELETE", "/stats/range") => stats_range(query, true).await,
        ("GET", "/queue") => json_response(200, state.queue.detailed_status()),
        ("GET", "/history/stats") => history_filtered(query, HistoryView::Stats).await,
        ("GET", "/history/models") => history::models()
            .await
            .map(|m| json_response(200, json!(m)))
            .unwrap_or_else(|e| api_error("INTERNAL_ERROR", Some(&e.to_string()), None, false)),
        ("GET", "/history") => history_filtered(query, HistoryView::List).await,
        ("DELETE", "/history") => delete_history(query, body).await,
        ("GET", p) if p.starts_with("/history/") && p.contains("/media/") => {
            history_media(state, p).await
        }
        ("POST", p) if p.contains("/retry-media") => retry_media(state, p, body).await,
        ("GET", p) if p.starts_with("/history/") => {
            let id = p.trim_start_matches("/history/");
            match history::detail(id).await {
                Ok(Some(r)) => json_response(200, r),
                Ok(None) => api_error("NOT_FOUND", Some("记录不存在"), Some(404), false),
                Err(e) => api_error("INTERNAL_ERROR", Some(&e.to_string()), None, false),
            }
        }
        // 路径存在但方法不受支持：Node 原版返回 405 空 body（对应 http.STATUS_CODES 的 sendStatus）
        (_m, p) if known_admin_path(p) => {
            Response::builder().status(405).body(Body::empty()).unwrap()
        }
        _ => Response::builder()
            .status(404)
            .body(Body::from(r#"{"error":"Not Found"}"#))
            .unwrap(),
    }
}

/// 路径本身已注册（任意方法命中过某条路由）时，方法不匹配返回 405 而非 404。
fn known_admin_path(path: &str) -> bool {
    const EXACT: &[&str] = &[
        "/status",
        "/restart",
        "/stop",
        "/vnc/status",
        "/cache/clear",
        "/logs",
        "/adapters",
        "/queue",
        "/stats",
        "/stats/range",
        "/history",
        "/history/models",
        "/history/stats",
        "/data-folders",
        "/data-folders/delete",
        "/config/server",
        "/config/browser",
        "/config/instances",
        "/config/workers",
        "/config/adapters",
        "/config/pool",
        "/browser/restart",
    ];
    if EXACT.contains(&path) {
        return true;
    }
    path.starts_with("/config/")
        || path.starts_with("/history/")
        || path.contains("/retry-media")
        || path.contains("/media/")
}

/// history::list / history::stats 已是 async，用枚举选择视图而不是传 fn 指针。
#[derive(Clone, Copy)]
enum HistoryView {
    List,
    Stats,
}

async fn history_filtered(query: &str, view: HistoryView) -> Response {
    let q = url_query(query);
    let get = |k: &str| q.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone());
    let filter = ListFilter {
        page: get("page").and_then(|v| v.parse().ok()).unwrap_or(1),
        page_size: get("pageSize").and_then(|v| v.parse().ok()).unwrap_or(20),
        status: get("status"),
        model: get("model"),
        search: get("search"),
        start_date: get("startDate"),
        end_date: get("endDate"),
    };
    let result = match view {
        HistoryView::List => history::list(&filter).await,
        HistoryView::Stats => history::stats(&filter).await,
    };
    result
        .map(|v| json_response(200, v))
        .unwrap_or_else(|e| api_error("INTERNAL_ERROR", Some(&e.to_string()), None, false))
}

async fn delete_history(query: &str, body: &[u8]) -> Response {
    let q = url_query(query);
    let get = |k: &str| q.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone());
    if let (Some(s), Some(e)) = (get("startDate"), get("endDate")) {
        return history::delete_by_date_range(&s, &e)
            .await
            .map(|n| json_response(200, json!({"success": true, "deleted": n})))
            .unwrap_or_else(|e| api_error("INTERNAL_ERROR", Some(&e.to_string()), None, false));
    }
    let b: Value = serde_json::from_slice(body).unwrap_or(json!({}));
    if let Some(ids) = b.get("ids").and_then(Value::as_array) {
        let ids: Vec<String> = ids
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        return history::delete_ids(&ids)
            .await
            .map(|n| json_response(200, json!({"success": true, "deleted": n})))
            .unwrap_or_else(|e| api_error("INTERNAL_ERROR", Some(&e.to_string()), None, false));
    }
    api_error(
        "INVALID_REQUEST_BODY",
        Some("缺少 ids 数组或日期范围参数"),
        None,
        false,
    )
}

async fn history_media(state: &AppState, path: &str) -> Response {
    let filename = path.rsplit('/').next().unwrap_or("");
    if filename.is_empty() || filename.contains("..") {
        return api_error("INVALID_REQUEST_BODY", Some("非法路径"), Some(400), false);
    }
    let file = state.data_dir.join("history/media").join(filename);
    // 整文件读取（含视频，可能很大）放阻塞线程池
    let bytes = match tokio::task::spawn_blocking({
        let file = file.clone();
        move || std::fs::read(&file)
    })
    .await
    {
        Ok(Ok(bytes)) => bytes,
        _ => return api_error("NOT_FOUND", Some("文件不存在"), Some(404), false),
    };
    let mime = match file.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        _ => "application/octet-stream",
    };
    Response::builder()
        .status(200)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CACHE_CONTROL, "public, max-age=31536000")
        .body(Body::from(bytes))
        .unwrap()
}

async fn retry_media(state: &AppState, path: &str, body: &[u8]) -> Response {
    let id = path
        .trim_start_matches("/history/")
        .trim_end_matches("/retry-media");
    let b: Value = serde_json::from_slice(body).unwrap_or(json!({}));
    let index = b.get("mediaIndex").and_then(Value::as_u64).unwrap_or(0) as usize;
    let record = match history::detail(id).await {
        Ok(Some(r)) => r,
        _ => return api_error("NOT_FOUND", Some("记录不存在"), Some(404), false),
    };
    let media = record["response_media"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let Some(item) = media.get(index) else {
        return api_error("INTERNAL_ERROR", Some("媒体索引无效"), None, false);
    };
    // 已下载且文件还在：无需重试（history.js retryMediaDownload 同款短路）
    if item.get("status").and_then(Value::as_str) == Some("downloaded") {
        let exists = item
            .get("localPath")
            .and_then(Value::as_str)
            .is_some_and(|p| Path::new(p).is_file());
        if exists {
            return json_response(200, json!({"success": true, "message": "媒体已下载"}));
        }
    }
    let url = item
        .get("originalUrl")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if url.is_empty() {
        return api_error(
            "INTERNAL_ERROR",
            Some("无原始 URL，无法重试下载"),
            None,
            false,
        );
    }

    let typed = crate::typed::Config::from_value(&state.config);
    let retries = if typed.backend.pool.failover.img_dl_retry.unwrap_or(false) {
        typed
            .backend
            .pool
            .failover
            .img_dl_retry_max_retries
            .unwrap_or(3)
    } else {
        1
    };

    // 优先浏览器上下文下载；桥不可用（未初始化/出错）时回退裸 HTTP（history.js 后备方案）
    let mut data_uri: Option<String> = None;
    match state.bridge.download_via_context(&url, retries).await {
        Ok(v) if v.get("error").is_none() => {
            if let Some(path) = v.get("path").and_then(Value::as_str) {
                let mime = v
                    .get("mime")
                    .and_then(Value::as_str)
                    .unwrap_or("application/octet-stream");
                match std::fs::read(path) {
                    Ok(bytes) => {
                        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
                        data_uri = Some(format!("data:{mime};base64,{b64}"));
                    }
                    Err(e) => {
                        crate::logfmt::warn("历史记录", &format!("读取桥下载的媒体失败: {e}"))
                    }
                }
            }
        }
        Ok(v) => crate::logfmt::warn(
            "历史记录",
            &format!(
                "浏览器下载失败，回退 HTTP: {}",
                v["error"].as_str().unwrap_or("未知")
            ),
        ),
        Err(e) => crate::logfmt::warn("历史记录", &format!("浏览器下载失败，回退 HTTP: {e}")),
    }
    if data_uri.is_none() {
        match http_download(&url).await {
            Ok((mime, bytes)) => {
                let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
                data_uri = Some(format!("data:{mime};base64,{b64}"));
            }
            Err(e) => return api_error("INTERNAL_ERROR", Some(&e), None, false),
        }
    }
    let data_uri = data_uri.unwrap();

    // 落盘到 history/media 并更新记录（history.js saveMediaToFile + updateRecord）
    match history::save_data_uri(&data_uri, id).await {
        Ok(saved) => {
            let mut new_media = media;
            new_media[index] = json!({
                "type": saved.get("type"),
                "originalUrl": url,
                "localPath": saved.get("localPath"),
                "status": "downloaded",
            });
            let media_json = serde_json::to_string(&new_media).unwrap_or_default();
            if let Err(e) = history::update_record(
                id,
                &RecordUpdate {
                    status: None,
                    response_text: None,
                    reasoning_content: None,
                    response_media: Some(media_json.as_str()),
                    error_message: None,
                    duration_ms: None,
                },
            )
            .await
            {
                crate::logfmt::error("历史记录", &format!("更新媒体记录失败: {e}"));
            }
            json_response(200, json!({"success": true, "message": "下载成功"}))
        }
        Err(e) => api_error(
            "INTERNAL_ERROR",
            Some(&format!("下载失败: {e}")),
            None,
            false,
        ),
    }
}

/// 裸 HTTP 下载后备（history.js 对 Pool 未初始化时的 fallback）：60s 超时 + 浏览器 UA。
async fn http_download(url: &str) -> Result<(String, Vec<u8>), String> {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    let client = CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap_or_default()
    });
    let resp = client
        .get(url)
        .header(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
        )
        .send()
        .await
        .map_err(|e| format!("下载失败: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!(
            "下载失败: HTTP {}（可能需要认证）",
            status.as_u16()
        ));
    }
    let mime = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.split(';')
                .next()
                .unwrap_or("application/octet-stream")
                .trim()
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "application/octet-stream".to_string());
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("下载失败: {e}"))?
        .to_vec();
    Ok((mime, bytes))
}

async fn stats_range(query: &str, clear: bool) -> Response {
    let q = url_query(query);
    let get = |k: &str| q.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone());
    let (Some(start), Some(end)) = (get("start"), get("end")) else {
        return api_error(
            "INVALID_REQUEST_BODY",
            Some("缺少 start 或 end 参数"),
            Some(400),
            false,
        );
    };
    if clear {
        stats::clear_range(&start, &end)
            .await
            .map(|n| json_response(200, json!({"success": true, "removed": n})))
            .unwrap_or_else(|e| api_error("INTERNAL_ERROR", Some(&e.to_string()), None, false))
    } else {
        stats::range(&start, &end)
            .await
            .map(|(s, f, d)| json_response(200, json!({"success": s, "failed": f, "days": d})))
            .unwrap_or_else(|e| api_error("INTERNAL_ERROR", Some(&e.to_string()), None, false))
    }
}

/// 配置写回：读现有 YAML → 按 WebUI 字段映射后写回（与原版一致，注释不保留）。
async fn save_config(state: &AppState, path: &str, body: &[u8]) -> Response {
    // Serialize the complete read/merge/validate/write transaction. Acquiring this
    // before reading prevents concurrent patches from overwriting one another.
    static CONFIG_SAVE: OnceLock<Arc<tokio::sync::Mutex<()>>> = OnceLock::new();
    let save_lock = CONFIG_SAVE
        .get_or_init(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone();
    let _guard = save_lock.lock_owned().await;
    let section = path.trim_start_matches("/config/");
    let patch: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return api_error("INVALID_REQUEST_BODY", None, None, false),
    };
    // 文件读取 + YAML 解析 + 校验 + 补丁应用 + 序列化是纯计算与文件 IO，
    // 整体放阻塞线程池；错误分支用枚举带回，映射保持与原实现逐条一致。
    enum Prepared {
        NotMapping,
        NotFound,
        Invalid(Vec<String>),
        SerializeError(String),
        Serialized(String),
    }
    let cfg_path = state.config_path.clone();
    let section = section.to_string();
    let prepared = match tokio::task::spawn_blocking(move || {
        let text = std::fs::read_to_string(&cfg_path).unwrap_or_default();
        let mut yaml: serde_yaml::Value =
            serde_yaml::from_str(&text).unwrap_or(serde_yaml::Value::Null);
        let patch_yaml: serde_yaml::Value =
            serde_json::from_value(patch.clone()).unwrap_or(serde_yaml::Value::Null);

        if yaml.as_mapping().is_none() {
            return Prepared::NotMapping;
        }
        // 先按原版 validator.js 逐条校验，通过后才允许写盘。
        let adapter_ids = crate::config_patch::adapter_ids();
        let errors = match section.as_str() {
            "server" => crate::config_patch::validate_server_patch(&patch),
            "browser" => crate::config_patch::validate_browser_patch(&patch),
            "instances" | "workers" => {
                let list = patch
                    .get("instances")
                    .and_then(Value::as_array)
                    .map(|a| a.to_vec())
                    .or_else(|| patch.as_array().map(|a| a.to_vec()))
                    .unwrap_or_default();
                crate::config_patch::validate_instances_patch(&list, &adapter_ids)
            }
            "adapters" => crate::config_patch::validate_adapters_patch(&patch),
            "pool" => crate::config_patch::validate_pool_patch(&patch),
            _ => return Prepared::NotFound,
        };
        if !errors.is_empty() {
            return Prepared::Invalid(errors);
        }
        match section.as_str() {
            "server" => crate::config_patch::apply_server_patch(&mut yaml, &patch),
            "browser" => crate::config_patch::apply_browser_patch(&mut yaml, &patch),
            "instances" | "workers" => {
                // WebUI 直接 POST 实例数组；也兼容 {instances: [...]}
                let list = patch_yaml
                    .get("instances")
                    .cloned()
                    .or_else(|| patch_yaml.as_sequence().map(|_| patch_yaml.clone()))
                    .unwrap_or(serde_yaml::Value::Sequence(vec![]));
                yaml["backend"]["pool"]["instances"] = list;
            }
            "adapters" => {
                if let (Some(dst), Some(src)) = (
                    yaml["backend"]["adapter"].as_mapping_mut(),
                    patch_yaml.as_mapping(),
                ) {
                    for (k, v) in src {
                        dst.insert(k.clone(), v.clone());
                    }
                } else {
                    yaml["backend"]["adapter"] = patch_yaml;
                }
            }
            "pool" => crate::config_patch::apply_pool_patch(&mut yaml, &patch),
            _ => return Prepared::NotFound,
        }
        // 写回前用加载器校验；失败返回原版文案（在调用方执行）。
        match serde_yaml::to_string(&yaml) {
            Ok(s) => Prepared::Serialized(s),
            Err(e) => Prepared::SerializeError(e.to_string()),
        }
    })
    .await
    {
        Ok(p) => p,
        Err(e) => {
            return api_error(
                "INTERNAL_ERROR",
                Some(&format!("配置处理失败: {e}")),
                None,
                false,
            )
        }
    };
    let serialized = match prepared {
        Prepared::NotMapping => {
            return api_error("INTERNAL_ERROR", Some("配置文件不是对象"), None, false)
        }
        Prepared::NotFound => return api_error("NOT_FOUND", None, None, false),
        Prepared::Invalid(errors) => {
            return api_error(
                "INVALID_REQUEST_BODY",
                Some(&format!("配置校验失败: {}", errors.join("; "))),
                Some(400),
                false,
            )
        }
        Prepared::SerializeError(e) => {
            return api_error(
                "INTERNAL_ERROR",
                Some(&format!("配置序列化失败: {e}")),
                None,
                false,
            )
        }
        Prepared::Serialized(s) => s,
    };
    // 原子写：临时文件 + rename，崩溃不留半截配置；互斥锁防止并发保存互相覆盖。
    let tmp = config_tmp_path(&state.config_path);
    let cfg_path = state.config_path.clone();
    let orig = std::fs::read_to_string(&cfg_path).unwrap_or_default();
    if let Err(e) = std::fs::write(&tmp, &serialized) {
        let _ = std::fs::remove_file(&tmp);
        return api_error(
            "INTERNAL_ERROR",
            Some(&format!("配置写入失败: {e}")),
            None,
            false,
        );
    }
    if let Err(e) = std::fs::rename(&tmp, &cfg_path) {
        let _ = std::fs::remove_file(&tmp);
        return api_error(
            "INTERNAL_ERROR",
            Some(&format!("配置写入失败: {e}")),
            None,
            false,
        );
    }
    // 用真实 src-root 重新加载校验，确保与启动路径一致
    if let Err(ConfigError(e)) = config::load_config_in(&state.src_root, &state.data_dir) {
        if let Err(restore) =
            std::fs::write(&tmp, &orig).and_then(|_| std::fs::rename(&tmp, &cfg_path))
        {
            crate::logfmt::error("服务器", &format!("配置回滚失败: {restore}"));
        }
        return api_error(
            "INVALID_REQUEST_BODY",
            Some(&format!("配置校验失败: {e}")),
            Some(400),
            false,
        );
    }
    json_response(
        200,
        json!({"success": true, "message": "配置已保存，请重启服务生效"}),
    )
}

fn config_tmp_path(cfg_path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.tmp", cfg_path.display()))
}

// ==================== 与 WebUI 对齐的视图 ====================

fn server_config_view(config: &Value) -> Value {
    json!({
        "port": config::port_of(config),
        "authToken": config::auth_of(config),
        "keepaliveMode": config::keepalive_mode(config),
        "logLevel": config.get("logLevel").and_then(Value::as_str).unwrap_or("info"),
        "imageMarkdown": config::image_markdown(config),
        "queueBuffer": config::queue_buffer(config),
        "imageLimit": config::image_limit(config),
    })
}

fn browser_config_view(config: &Value) -> Value {
    let b = config.get("browser").cloned().unwrap_or(json!({}));
    let proxy = b.get("proxy").cloned().unwrap_or(json!({}));
    let css = b.get("cssInject").cloned().unwrap_or(json!({}));
    let cam = b.get("camoufox").cloned().unwrap_or(json!({}));
    let cc = b.get("clearcote").cloned().unwrap_or(json!({}));
    let user = proxy.get("user").and_then(Value::as_str).unwrap_or("");
    let pass = proxy.get("passwd").and_then(Value::as_str).unwrap_or("");
    json!({
        "path": b.get("path").and_then(Value::as_str).unwrap_or(""),
        "engine": b.get("engine").and_then(Value::as_str).unwrap_or("camoufox"),
        "headless": b.get("headless").and_then(Value::as_bool).unwrap_or(false),
        "fission": b.get("fission").and_then(Value::as_bool).unwrap_or(true),
        "humanizeCursor": b.get("humanizeCursor").cloned().unwrap_or(json!("camou")),
        "ffVersion": b.get("ffVersion").cloned().unwrap_or(Value::Null),
        "camoufox": {
            "mainWorldEval": cam.get("mainWorldEval").and_then(Value::as_bool).unwrap_or(false),
            "enableCache": cam.get("enableCache").and_then(Value::as_bool).unwrap_or(false),
            "disableInstantAnimations": cam.get("disableInstantAnimations").and_then(Value::as_bool).unwrap_or(false),
            "humanizeMaxTime": cam.get("humanizeMaxTime").cloned().unwrap_or(json!(1.5)),
            "blockWebRtc": cam.get("blockWebRtc").and_then(Value::as_bool).unwrap_or(true),
            "geoip": cam.get("geoip").and_then(Value::as_bool).unwrap_or(true),
            "locale": cam.get("locale").cloned().unwrap_or(Value::Null),
            "certificates": cam.get("certificates").cloned().unwrap_or(json!([])),
            "certificatePaths": cam.get("certificatePaths").cloned().unwrap_or(json!([])),
        },
        "clearcote": {
            "path": cc.get("path").and_then(Value::as_str).unwrap_or(""),
            "platform": cc.get("platform").and_then(Value::as_str).unwrap_or("auto"),
            "brand": cc.get("brand").and_then(Value::as_str).unwrap_or("Chrome"),
            "fingerprintProfile": cc.get("fingerprintProfile").and_then(Value::as_str).unwrap_or(""),
            "timezone": cc.get("timezone").and_then(Value::as_str).unwrap_or(""),
            "acceptLanguage": cc.get("acceptLanguage").and_then(Value::as_str).unwrap_or(""),
            "geoip": cc.get("geoip").and_then(Value::as_bool).unwrap_or(true),
            "humanize": cc.get("humanize").and_then(Value::as_bool).unwrap_or(true),
            "webrtcIp": cc.get("webrtcIp").and_then(Value::as_str).unwrap_or(""),
            "sandbox": cc.get("sandbox").and_then(Value::as_bool).unwrap_or(true),
            "allowDetectedLicense": cc.get("allowDetectedLicense").and_then(Value::as_bool).unwrap_or(false),
            "args": cc.get("args").cloned().unwrap_or(json!([])),
        },
        "cssInject": {
            "animation": css.get("animation").and_then(Value::as_bool).unwrap_or(false),
            "filter": css.get("filter").and_then(Value::as_bool).unwrap_or(false),
            "font": css.get("font").and_then(Value::as_bool).unwrap_or(false),
        },
        "proxy": {
            "enable": proxy.get("enable").and_then(Value::as_bool).unwrap_or(false),
            "type": proxy.get("type").and_then(Value::as_str).unwrap_or("http"),
            "host": proxy.get("host").and_then(Value::as_str).unwrap_or(""),
            "port": proxy.get("port").and_then(Value::as_u64).unwrap_or(0),
            "auth": !user.is_empty() || !pass.is_empty(),
            "username": user,
            "password": pass,
        },
    })
}

fn instances_config_view(config: &Value) -> Value {
    let instances = crate::typed::Config::from_value(config)
        .backend
        .pool
        .instances
        .unwrap_or_default();
    Value::Array(
        instances
            .iter()
            .map(|inst| {
                let proxy = inst.get("proxy").filter(|p| p.is_object()).map(|p| {
                    json!({
                        "enable": p.get("enable").and_then(Value::as_bool).unwrap_or(false),
                        "type": p.get("type").and_then(Value::as_str).unwrap_or("http"),
                        "host": p.get("host").and_then(Value::as_str).unwrap_or(""),
                        "port": p.get("port").and_then(Value::as_u64).unwrap_or(0),
                    })
                });
                let workers = inst
                    .get("workers")
                    .and_then(Value::as_array)
                    .map(|ws| {
                        ws.iter().map(|w| json!({
                "name": w.get("name").cloned().unwrap_or(Value::Null),
                "type": w.get("type").cloned().unwrap_or(Value::Null),
                "mergeTypes": w.get("mergeTypes").cloned().unwrap_or(json!([])),
                "mergeMonitor": w.get("mergeMonitor").cloned().filter(|v| !v.is_null()),
            })).collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                json!({
                    "name": inst.get("name").cloned().unwrap_or(Value::Null),
                    "engine": inst.get("engine").cloned().filter(|v| v.is_string()),
                    "userDataMark": inst.get("userDataMark").cloned().filter(|v| v.is_string()),
                    "proxy": proxy,
                    "workers": workers,
                })
            })
            .collect(),
    )
}

fn pool_config_view(config: &Value) -> Value {
    let typed = crate::typed::Config::from_value(config);
    let f = &typed.backend.pool.failover;
    json!({
        "strategy": typed.pool_strategy(),
        "waitTimeout": typed.backend.pool.wait_timeout.map(|ms| ms / 1000).unwrap_or(120),
        "failover": {
            "enabled": f.enabled.unwrap_or(true),
            "maxRetries": f.max_retries.unwrap_or(2),
            "imgDlRetry": f.img_dl_retry.unwrap_or(false),
            "imgDlRetryMaxRetries": f.img_dl_retry_max_retries.unwrap_or(2),
        }
    })
}

/// WebUI 适配器页要数组：模型 id 是字符串，并带上已保存的 modelFilter。
fn adapters_meta(bridge_value: &Value, config: &Value) -> Value {
    let list = bridge_value
        .get("adapters")
        .and_then(Value::as_array)
        .or_else(|| bridge_value.as_array())
        .cloned()
        .unwrap_or_default();
    let saved = crate::typed::Config::from_value(config).backend.adapter;
    Value::Array(
        list.iter()
            .map(|a| {
                let id = a.get("id").and_then(Value::as_str).unwrap_or("");
                let models: Vec<String> = a
                    .get("models")
                    .and_then(Value::as_array)
                    .map(|ms| {
                        ms.iter()
                            .filter_map(|m| {
                                m.get("id")
                                    .and_then(Value::as_str)
                                    .or_else(|| m.as_str())
                                    .map(str::to_string)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let filter = saved
                    .get(id)
                    .and_then(|c| c.get("modelFilter"))
                    .cloned()
                    .unwrap_or(json!({"mode": "blacklist", "list": []}));
                json!({
                    "id": id,
                    "displayName": a.get("displayName").and_then(Value::as_str).unwrap_or(id),
                    "description": a.get("description").and_then(Value::as_str).unwrap_or(""),
                    "modelCount": models.len(),
                    "models": models,
                    "modelFilter": filter,
                    "configSchema": a.get("configSchema").cloned().unwrap_or(json!([])),
                })
            })
            .collect(),
    )
}

/// 按 owned_by 聚合正在提供的模型，并挂上配置里的 worker。
fn provider_list(models: &[Value], config: &Value) -> Vec<Value> {
    use std::collections::BTreeMap;
    let mut by: BTreeMap<String, Value> = BTreeMap::new();
    for m in models {
        let owner = m
            .get("owned_by")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let bucket = by.entry(owner.clone()).or_insert_with(|| {
            json!({
                "id": owner, "name": owner, "models": [], "modelCount": 0,
                "textCount": 0, "imageCount": 0, "workers": [], "enabled": true,
            })
        });
        let is_text = m.get("type").and_then(Value::as_str) == Some("text");
        bucket["models"].as_array_mut().unwrap().push(json!({
            "id": m.get("id").cloned().unwrap_or(Value::Null),
            "type": if is_text { "text" } else { m.get("type").and_then(Value::as_str).unwrap_or("image") },
            "image_policy": m.get("image_policy").cloned().or_else(|| m.get("imagePolicy").cloned()),
        }));
        bucket["modelCount"] = json!(bucket["modelCount"].as_u64().unwrap_or(0) + 1);
        let key = if is_text { "textCount" } else { "imageCount" };
        bucket[key] = json!(bucket[key].as_u64().unwrap_or(0) + 1);
    }
    let workers = crate::typed::Config::from_value(config)
        .backend
        .pool
        .workers
        .unwrap_or_default();
    for w in &workers {
        let types: Vec<String> = if w.get("type").and_then(Value::as_str) == Some("merge") {
            w.get("mergeTypes")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        } else {
            w.get("type")
                .and_then(Value::as_str)
                .map(|s| vec![s.to_string()])
                .unwrap_or_default()
        };
        for t in types {
            let bucket = by.entry(t.clone()).or_insert_with(|| {
                json!({
                    "id": t, "name": t, "models": [], "modelCount": 0,
                    "textCount": 0, "imageCount": 0, "workers": [], "enabled": true,
                })
            });
            bucket["workers"].as_array_mut().unwrap().push(json!({
                "name": w.get("name").cloned().unwrap_or(Value::Null),
                "instance": w.get("instanceName").cloned().unwrap_or(Value::Null),
                "busy": false,
                "busyCount": 0,
            }));
        }
    }
    by.into_values()
        .map(|mut p| {
            let running = p["workers"].as_array().map(|a| a.len()).unwrap_or(0);
            let model_count = p["modelCount"].as_u64().unwrap_or(0);
            p["runningWorkers"] = json!(running);
            p["status"] = json!(if running > 0 {
                "active"
            } else if model_count > 0 {
                "registered"
            } else {
                "idle"
            });
            p
        })
        .collect()
}

fn referenced_engine_names(config: &Value) -> Vec<String> {
    let mut names = Vec::new();
    let mut push = |s: &str| {
        if !names.iter().any(|n| n == s) {
            names.push(s.to_string());
        }
    };
    let typed = crate::typed::Config::from_value(config);
    push(&typed.browser_engine());
    if let Some(ws) = typed.backend.pool.workers.as_deref() {
        for w in ws {
            if let Some(engine) = w.get("engine").and_then(Value::as_str) {
                push(engine);
            }
        }
    }
    names
}

/// 只读 `src-root/camoufox/version.json`。文件缺失时返回 null，不拼接其他路径。
fn read_camoufox_version() -> Value {
    let file = src_root().join("camoufox").join("version.json");
    let Ok(text) = std::fs::read_to_string(&file) else {
        return Value::Null;
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return Value::Null;
    };
    let version = v.get("version").and_then(Value::as_str).unwrap_or("");
    let Some(major_str) = version.split('.').next() else {
        return Value::Null;
    };
    let Ok(major) = major_str.parse::<u32>() else {
        return Value::Null;
    };
    let release = v.get("release").and_then(Value::as_str).unwrap_or("");
    let full = if release.is_empty() {
        version.to_string()
    } else {
        format!("{version}-{release}")
    };
    json!({ "version": version, "release": release, "major": major, "full": full })
}

/// 原仓库根目录（含 camoufox/、node_modules/）：来自 --src-root / WEBAI2API_SRC_ROOT。
/// 拒绝含 `..` 段的取值，否则拼出的读取目标可能落到预期目录之外。
fn src_root() -> PathBuf {
    for var in ["WEBAI2API_SRC_ROOT", "CAMOUFOX_INSTALL_DIR"] {
        if let Ok(dir) = std::env::var(var) {
            let p = PathBuf::from(&dir);
            let no_parent = !p
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir));
            if !dir.is_empty() && no_parent {
                return if var == "CAMOUFOX_INSTALL_DIR" {
                    p.parent().map(|d| d.to_path_buf()).unwrap_or(p)
                } else {
                    p
                };
            }
        }
    }
    std::env::current_dir().unwrap_or_default()
}

/// Clearcote SDK 版本（引擎为 clearcote 时由桥侧提供；此处只做文件层探测）。
fn read_clearcote_sdk_version() -> Value {
    let dir = src_root().join("node_modules").join("clearcote");
    if !dir.exists() {
        return Value::Null;
    }
    let Ok(text) = std::fs::read_to_string(dir.join("package.json")) else {
        return Value::Null;
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return Value::Null;
    };
    match v.get("version").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => json!(s),
        _ => Value::Null,
    }
}

// ==================== 辅助 ====================

fn json_response(status: u16, body: Value) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap_or_default()))
        .unwrap()
}

fn json_response_status((status, body): (u16, Value)) -> Response {
    json_response(status, body)
}

fn api_error(
    code: &str,
    message: Option<&str>,
    status_override: Option<u16>,
    streaming: bool,
) -> Response {
    let d = error_detail(code);
    let body = respond::openai_error_body(code, message);
    if streaming {
        let payload = format!("{}{}", respond::sse_event(&body), respond::sse_done());
        Response::builder()
            .status(200)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from(payload))
            .unwrap()
    } else {
        json_response(status_override.unwrap_or(d.status), body)
    }
}

async fn read_body(req: axum::extract::Request) -> Vec<u8> {
    axum::body::to_bytes(req.into_body(), 64 * 1024 * 1024)
        .await
        .unwrap_or_default()
        .to_vec()
}

fn url_query(query: &str) -> Vec<(String, String)> {
    url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect()
}

/// 与 Node `crypto.randomUUID().slice(0, 8)` 对齐：8 位随机 hex。
/// 原时间戳方案约 4.3 秒回绕一次，碰撞时记录会被静默丢弃。
fn short_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    let seed = std::collections::hash_map::RandomState::new();
    format!("{:08x}", seed.build_hasher().finish())
}

/// API token / VNC token 的常量时间比较：避免按字节提前返回泄露前缀匹配长度。
/// 长度不同也走完整比较路径，仅用标志位记录差异。
pub(crate) fn token_eq(a: &str, b: &str) -> bool {
    let (x, y) = (a.as_bytes(), b.as_bytes());
    let mut diff = (x.len() ^ y.len()) as u64;
    for i in 0..x.len().max(y.len()) {
        let px = x.get(i).copied().unwrap_or(0) as u64;
        let py = y.get(i).copied().unwrap_or(0) as u64;
        diff |= px ^ py;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_eq_semantics() {
        assert!(token_eq("secret-token", "secret-token"));
        assert!(!token_eq("secret-token", "secret-tokeN"));
        // 长度不同：前缀相同也不相等（这是 == 语义的硬性要求）
        assert!(!token_eq("secret", "secret-token"));
        assert!(!token_eq("secret-token", ""));
        assert!(!token_eq("", "x"));
        assert!(token_eq("", ""));
    }

    #[test]
    fn admin_views_match_webui_shape() {
        let config = json!({
            "logLevel": "info",
            "server": { "port": 3100, "auth": "", "keepalive": { "mode": "comment" }, "imageMarkdown": false },
            "queue": { "queueBuffer": 2, "imageLimit": 5 },
            "browser": {
                "engine": "camoufox", "path": "",
                "proxy": { "enable": false, "type": "http", "host": "127.0.0.1", "port": 7890, "user": "u", "passwd": "p" }
            },
            "backend": {
                "pool": {
                    "strategy": "least_busy",
                    "waitTimeout": 120000,
                    "instances": [{ "name": "browser_default", "workers": [{ "name": "default", "type": "lmarena" }] }],
                    "workers": [{ "name": "default", "type": "lmarena", "instanceName": "browser_default", "engine": "camoufox" }]
                },
                "adapter": { "lmarena": { "returnUrl": false, "modelFilter": { "mode": "whitelist", "list": ["a"] } } }
            }
        });
        let server = server_config_view(&config);
        assert_eq!(server["authToken"], "");
        assert_eq!(server["keepaliveMode"], "comment");
        assert_eq!(server["logLevel"], "info");
        let instances = instances_config_view(&config);
        assert!(instances.is_array());
        assert_eq!(instances[0]["workers"][0]["mergeTypes"], json!([]));
        assert_eq!(pool_config_view(&config)["waitTimeout"], 120);
        let browser = browser_config_view(&config);
        assert_eq!(browser["proxy"]["username"], "u");
        assert_eq!(browser["proxy"]["auth"], true);
        let adapters = adapters_meta(
            &json!({"adapters": [{
                "id": "lmarena", "displayName": "LMArena", "models": [{"id": "m1"}, {"id": "m2"}]
            }]}),
            &config,
        );
        assert!(adapters.is_array());
        assert_eq!(adapters[0]["models"], json!(["m1", "m2"]));
        assert_eq!(adapters[0]["modelFilter"]["mode"], "whitelist");
        let providers = provider_list(
            &[
                json!({"id": "m1", "owned_by": "lmarena", "type": "image", "image_policy": "optional"}),
            ],
            &config,
        );
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0]["status"], "active");
        assert_eq!(providers[0]["runningWorkers"], 1);
    }
}
