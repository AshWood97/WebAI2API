//! 引擎桥客户端：通过 Unix socket 以 JSON 行协议调用 Node 桥进程。
//! 协议见 `bridge/bridge.mjs`。所有调用串行（浏览器池本身受队列并发控制，
//! 桥内再并行没有收益），用一条后台读循环分发响应。

use crate::errors::BridgeError;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};

type ReplyTx = oneshot::Sender<Result<Value, BridgeError>>;

struct BridgeInner {
    /// UnboundedSender 本身 Clone+Send，无需 Mutex 包装
    writer: mpsc::UnboundedSender<String>,
    pending: Mutex<HashMap<u64, ReplyTx>>,
    next_id: AtomicU64,
}

#[derive(Clone)]
pub struct Bridge {
    inner: Arc<BridgeInner>,
}

impl Bridge {
    /// 连接到已启动的桥。`on_event` 接收桥的反向事件（log/fatal/ready）。
    pub async fn connect<F>(sock: PathBuf, mut on_event: F) -> Result<Self, BridgeError>
    where
        F: FnMut(Value) + Send + 'static,
    {
        let stream = UnixStream::connect(&sock)
            .await
            .map_err(|e| BridgeError::Connect {
                path: sock.clone(),
                source: e,
            })?;
        let (read_half, mut write_half) = stream.into_split();

        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            while let Some(line) = rx.recv().await {
                if write_half.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if write_half.write_all(b"\n").await.is_err() {
                    break;
                }
                if write_half.flush().await.is_err() {
                    break;
                }
            }
        });

        let bridge = Self {
            inner: Arc::new(BridgeInner {
                writer: tx,
                pending: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
            }),
        };

        let inner = Arc::clone(&bridge.inner);
        tokio::spawn(async move {
            let mut lines = BufReader::new(read_half).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                match msg.get("id").and_then(Value::as_u64) {
                    Some(id) => {
                        if let Some(reply) = inner.pending.lock().unwrap().remove(&id) {
                            let result = if let Some(err) = msg.get("error").and_then(Value::as_str)
                            {
                                Err(BridgeError::Remote(err.to_string()))
                            } else {
                                Ok(msg.get("result").cloned().unwrap_or(Value::Null))
                            };
                            let _ = reply.send(result);
                        }
                    }
                    None => on_event(msg),
                }
            }
            // 连接断开：所有等待中的调用失败
            for (_, reply) in inner.pending.lock().unwrap().drain() {
                let _ = reply.send(Err(BridgeError::Disconnected));
            }
        });
        Ok(bridge)
    }

    async fn call(
        &self,
        method: &str,
        params: Value,
        timeout: std::time::Duration,
    ) -> Result<Value, BridgeError> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().unwrap().insert(id, tx);
        let line = serde_json::to_string(&json!({"id": id, "method": method, "params": params}))?;
        self.inner
            .writer
            .send(line)
            .map_err(|_| BridgeError::WriteClosed)?;
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(BridgeError::ReplyChannelClosed),
            Err(_) => {
                self.inner.pending.lock().unwrap().remove(&id);
                Err(BridgeError::Timeout {
                    method: method.to_string(),
                })
            }
        }
    }

    pub async fn init(&self, config: &Value) -> Result<Value, BridgeError> {
        // 浏览器拉起可能很慢（首次下载内核、登录等待），给足 10 分钟
        self.call(
            "init",
            json!({"config": config}),
            std::time::Duration::from_secs(600),
        )
        .await
    }

    /// 启动预检（对齐 server.js 的 runPreflight）：依赖/内核/GeoIP 缺失时报错。
    pub async fn preflight(&self) -> Result<(), BridgeError> {
        self.call("preflight", json!({}), std::time::Duration::from_secs(120))
            .await
            .map(|_| ())
    }

    pub async fn generate(
        &self,
        id: &str,
        prompt: &str,
        image_paths: &[String],
        model_id: &str,
        reasoning: bool,
    ) -> Result<GenerateResult, BridgeError> {
        let result = self
            .call(
                "generate",
                json!({
                    "id": id,
                    "prompt": prompt,
                    "imagePaths": image_paths,
                    "modelId": model_id,
                    "reasoning": reasoning,
                }),
                std::time::Duration::from_secs(600),
            )
            .await?;
        Ok(parse_generate_result(&result))
    }

    pub async fn get_models(&self) -> Result<Value, BridgeError> {
        self.call("getModels", json!({}), std::time::Duration::from_secs(30))
            .await
    }

    /// 聚合查询：一次 IPC 取回模型表 + 指定模型的 type/imagePolicy（供 Rust 侧缓存）。
    pub async fn model_info(&self, model: &str) -> Result<Value, BridgeError> {
        self.call(
            "modelInfo",
            json!({"model": model}),
            std::time::Duration::from_secs(30),
        )
        .await
    }

    pub async fn get_image_policy(&self, model: &str) -> Result<String, BridgeError> {
        let v = self
            .call(
                "getImagePolicy",
                json!({"model": model}),
                std::time::Duration::from_secs(30),
            )
            .await?;
        Ok(v.get("policy")
            .and_then(Value::as_str)
            .unwrap_or("optional")
            .to_string())
    }

    pub async fn get_model_type(&self, model: &str) -> Result<String, BridgeError> {
        let v = self
            .call(
                "getModelType",
                json!({"model": model}),
                std::time::Duration::from_secs(30),
            )
            .await?;
        Ok(v.get("type")
            .and_then(Value::as_str)
            .unwrap_or("image")
            .to_string())
    }

    pub async fn get_cookies(
        &self,
        instance: Option<&str>,
        domain: Option<&str>,
    ) -> Result<Value, BridgeError> {
        self.call(
            "getCookies",
            json!({"instance": instance, "domain": domain}),
            std::time::Duration::from_secs(60),
        )
        .await
    }

    pub async fn navigate_to_monitor(&self) -> Result<(), BridgeError> {
        self.call(
            "navigateToMonitor",
            json!({}),
            std::time::Duration::from_secs(120),
        )
        .await
        .map(|_| ())
    }

    pub async fn restart_browser(&self) -> Result<Value, BridgeError> {
        self.call(
            "restartBrowser",
            json!({}),
            std::time::Duration::from_secs(300),
        )
        .await
    }

    pub async fn list_adapters(&self) -> Result<Value, BridgeError> {
        self.call(
            "listAdapters",
            json!({}),
            std::time::Duration::from_secs(30),
        )
        .await
    }

    pub async fn download_via_context(
        &self,
        url: &str,
        retries: u64,
    ) -> Result<Value, BridgeError> {
        self.call(
            "downloadViaContext",
            json!({"url": url, "retries": retries}),
            std::time::Duration::from_secs(180),
        )
        .await
    }

    pub async fn shutdown(&self) {
        let _ = self
            .call("shutdown", json!({}), std::time::Duration::from_secs(30))
            .await;
    }
}

/// 桥返回的生成结果。
#[derive(Debug, Clone)]
pub struct GenerateResult {
    pub text: Option<String>,
    pub image_path: Option<String>,
    pub image_mime: Option<String>,
    pub image_url: Option<String>,
    pub reasoning: Option<String>,
    pub error: Option<String>,
    pub code: Option<String>,
    pub retryable: bool,
}

fn parse_generate_result(v: &Value) -> GenerateResult {
    GenerateResult {
        text: v.get("text").and_then(Value::as_str).map(str::to_string),
        image_path: v
            .get("imagePath")
            .and_then(Value::as_str)
            .map(str::to_string),
        image_mime: v
            .get("imageMime")
            .and_then(Value::as_str)
            .map(str::to_string),
        image_url: v
            .get("imageUrl")
            .and_then(Value::as_str)
            .map(str::to_string),
        reasoning: v
            .get("reasoning")
            .and_then(Value::as_str)
            .map(str::to_string),
        error: v.get("error").and_then(Value::as_str).map(str::to_string),
        code: v.get("code").and_then(Value::as_str).map(str::to_string),
        retryable: v.get("retryable").and_then(Value::as_bool).unwrap_or(false),
    }
}

/// 启动 Node 桥子进程并等待其 ready 事件。返回 (子进程, Bridge)。
pub async fn spawn_bridge(
    node_bin: &str,
    bridge_script: &std::path::Path,
    src_root: &std::path::Path,
    sock: &std::path::Path,
    login: Option<&str>,
    temp_dir: &std::path::Path,
    extra_envs: &[(String, String)],
) -> Result<(tokio::process::Child, Bridge), BridgeError> {
    if sock.exists() {
        std::fs::remove_file(sock).ok();
    }
    let mut cmd = tokio::process::Command::new(node_bin);
    cmd.arg(bridge_script)
        .env("WEBAI2API_SRC_ROOT", src_root)
        .env("WEBAI2API_SOCK", sock)
        .env("WEBAI2API_LOGIN", login.unwrap_or(""))
        .env("CAMOUFOX_INSTALL_DIR", src_root.join("camoufox"))
        // 桥把生成结果的 data URI 图片落到该目录，/admin/cache/clear 才能覆盖到
        .env("WEBAI2API_TEMP_DIR", temp_dir)
        // 日志以 Rust 侧为唯一出口：抑制桥内 logger.js 的 console 重复输出
        .env("WEBAI2API_BRIDGE_QUIET", "1")
        // 桥 import 的原仓库配置器用 process.cwd() 找 data/config.yaml，必须跟 --src-root
        .current_dir(src_root)
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true);
    // 测试/嵌入场景的附加环境变量，作用域限定在桥子进程
    for (k, v) in extra_envs {
        cmd.env(k, v);
    }
    // setsid 使桥自成进程组：Rust 侧可整组击杀，回收 camoufox 等孙进程
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd.spawn().map_err(|e| BridgeError::Spawn { source: e })?;

    // 等待 socket 文件出现（桥 listen 之后）
    for _ in 0..100 {
        if sock.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    if !sock.exists() {
        return Err(BridgeError::SocketNotCreated);
    }

    let (ready_tx, ready_rx) = oneshot::channel();
    let mut ready_tx = Some(ready_tx);
    let bridge = Bridge::connect(sock.to_path_buf(), move |event| {
        if event.get("event").and_then(Value::as_str) == Some("ready") {
            if let Some(tx) = ready_tx.take() {
                let _ = tx.send(());
            }
        } else if event.get("event").and_then(Value::as_str) == Some("log") {
            let level = event.get("level").and_then(Value::as_str).unwrap_or("info");
            let module = event
                .get("module")
                .and_then(Value::as_str)
                .unwrap_or("桥接");
            let message = event.get("message").and_then(Value::as_str).unwrap_or("");
            crate::logfmt::log(level, module, message, &[], &[]);
        } else if event.get("event").and_then(Value::as_str) == Some("fatal") {
            let message = event
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("未知错误");
            crate::logfmt::error("桥接", &format!("引擎桥致命错误: {message}"));
        }
    })
    .await?;

    tokio::time::timeout(std::time::Duration::from_secs(30), ready_rx)
        .await
        .map_err(|_| BridgeError::ReadyTimeout)?
        .map_err(|_| BridgeError::ReadyChannelClosed)?;
    Ok((child, bridge))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_result_shapes() {
        let ok = parse_generate_result(&json!({"text": "hi", "reasoning": "r"}));
        assert_eq!(ok.text.as_deref(), Some("hi"));
        assert!(ok.error.is_none());
        let bad = parse_generate_result(
            &json!({"error": "boom", "code": "PAGE_CLOSED", "retryable": true}),
        );
        assert_eq!(bad.code.as_deref(), Some("PAGE_CLOSED"));
        assert!(bad.retryable);
    }
}
