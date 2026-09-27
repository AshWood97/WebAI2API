//! 任务队列，语义对应原 `src/server/queue.js`。
//! 非流式请求受 `maxConcurrent + queueBuffer` 限制（queueBuffer=0 表示不限制），
//! 流式请求永不拒绝（保留原行为）。生成期间每 3 秒发一次心跳，
//! 结束后一次性回放单个 chunk + [DONE]（伪流式，与原版一致）。

#[cfg(test)]
use crate::bridge::Bridge;
use crate::bridge::GenerateResult;
use crate::history::{self, NewRecord, RecordUpdate};
use crate::respond;
use crate::runtime::BackendRuntime;
use crate::stats;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, Semaphore};

/// 非流式任务的完成回执：(HTTP 状态码, 响应体)。Task 被独占消费，无需锁包装。
pub type TaskReply = Option<tokio::sync::oneshot::Sender<(u16, Value)>>;

pub struct Task {
    pub id: String,
    pub prompt: String,
    pub image_paths: Vec<PathBuf>,
    pub model_id: String,
    pub model_name: String,
    pub is_streaming: bool,
    pub reasoning: bool,
    /// 流式响应的写出端（SSE 字节）。
    pub sink: mpsc::UnboundedSender<String>,
    /// 非流式响应：完成后回传 (status, json)。
    pub reply: TaskReply,
    pub enqueued_at: std::time::Instant,
}

struct QueueInner {
    waiting: Mutex<Vec<TaskSnapshot>>,
    processing: Mutex<Vec<TaskSnapshot>>,
}

#[derive(Clone)]
struct TaskSnapshot {
    id: String,
    model: String,
    is_streaming: bool,
}

pub struct Queue {
    inner: Arc<QueueInner>,
    tx: mpsc::UnboundedSender<Task>,
    processing_count: Arc<AtomicU64>,
    /// 已通过准入、尚未进入 waiting 的请求数，堵住检查与入队之间的并发空隙。
    reserved: Arc<AtomicU64>,
    pub max_concurrent: u64,
    pub queue_buffer: u64,
}

impl Queue {
    pub fn new<B: Into<BackendRuntime>>(
        backend: B,
        max_concurrent: u64,
        queue_buffer: u64,
        keepalive_mode: String,
        image_markdown: bool,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel::<Task>();
        let inner = Arc::new(QueueInner {
            waiting: Mutex::new(Vec::new()),
            processing: Mutex::new(Vec::new()),
        });
        let processing_count = Arc::new(AtomicU64::new(0));
        let queue = Self {
            inner: Arc::clone(&inner),
            tx,
            processing_count: Arc::clone(&processing_count),
            reserved: Arc::new(AtomicU64::new(0)),
            max_concurrent,
            queue_buffer,
        };
        tokio::spawn(run_queue(
            rx,
            backend.into(),
            inner,
            processing_count,
            max_concurrent,
            keepalive_mode,
            image_markdown,
        ));
        queue
    }

    /// 非流式准入。先原子占位再校验，超限回滚：先检查后占位会让两个并发请求同时通过。
    pub fn try_reserve(&self) -> bool {
        if self.queue_buffer == 0 {
            return true;
        }
        let reserved_now = self.reserved.fetch_add(1, Ordering::AcqRel) + 1;
        let waiting = self.inner.waiting.lock().unwrap().len() as u64;
        let processing = self.processing_count.load(Ordering::Acquire);
        let limit = self.max_concurrent + self.queue_buffer;
        if processing + waiting + reserved_now > limit {
            self.reserved.fetch_sub(1, Ordering::AcqRel);
            return false;
        }
        true
    }

    pub fn cancel_reservation(&self) {
        if self.queue_buffer == 0 {
            return;
        }
        // 饱和递减：取消数超过占位数（调用方 bug）时停在 0，不让计数下溢回绕毒化准入
        let mut cur = self.reserved.load(Ordering::Acquire);
        loop {
            if cur == 0 {
                return;
            }
            match self.reserved.compare_exchange_weak(
                cur,
                cur - 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(x) => cur = x,
            }
        }
    }

    pub fn status_total(&self) -> u64 {
        self.processing_count.load(Ordering::Relaxed)
            + self.inner.waiting.lock().unwrap().len() as u64
    }

    pub fn enqueue(&self, task: Task) {
        // 非流式名额从"预留"转为"等待"；流式不受队列上限约束（保留原行为）
        if self.queue_buffer != 0 && !task.is_streaming {
            self.reserved.fetch_sub(1, Ordering::Relaxed);
        }
        self.inner.waiting.lock().unwrap().push(TaskSnapshot {
            id: task.id.clone(),
            model: task.model_name.clone(),
            is_streaming: task.is_streaming,
        });
        let _ = self.tx.send(task);
    }

    pub fn detailed_status(&self) -> Value {
        let snap = |list: &[TaskSnapshot]| -> Vec<Value> {
            list.iter()
                .map(|t| json!({"id": t.id, "model": t.model, "isStreaming": t.is_streaming}))
                .collect()
        };
        let processing = snap(&self.inner.processing.lock().unwrap());
        let waiting = snap(&self.inner.waiting.lock().unwrap());
        json!({
            "processing": processing.len(),
            "waiting": waiting.len(),
            "total": processing.len() + waiting.len(),
            "processingTasks": processing,
            "waitingTasks": waiting,
        })
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_queue(
    mut rx: mpsc::UnboundedReceiver<Task>,
    backend: BackendRuntime,
    inner: Arc<QueueInner>,
    processing_count: Arc<AtomicU64>,
    max_concurrent: u64,
    keepalive_mode: String,
    image_markdown: bool,
) {
    let sem = Arc::new(Semaphore::new(max_concurrent.max(1) as usize));
    while let Some(task) = rx.recv().await {
        let permit = sem.clone().acquire_owned().await.unwrap();
        // 先计入 processing 再移出 waiting：中间状态被双重计数，准入只会保守不会超限
        inner.processing.lock().unwrap().push(TaskSnapshot {
            id: task.id.clone(),
            model: task.model_name.clone(),
            is_streaming: task.is_streaming,
        });
        processing_count.fetch_add(1, Ordering::Relaxed);
        inner.waiting.lock().unwrap().retain(|t| t.id != task.id);
        let backend = backend.clone();
        let inner = Arc::clone(&inner);
        let processing_count = Arc::clone(&processing_count);
        let keepalive_mode = keepalive_mode.clone();
        let task_id = task.id.clone();
        tokio::spawn(async move {
            process_task(task, &backend, keepalive_mode, image_markdown).await;
            inner.processing.lock().unwrap().retain(|t| t.id != task_id);
            processing_count.fetch_sub(1, Ordering::Relaxed);
            drop(permit);
            // 队列清空时让浏览器回到监控页（queue.js processQueue 空闲判定）
            if processing_count.load(Ordering::Relaxed) == 0
                && inner.waiting.lock().unwrap().is_empty()
            {
                let _ = backend.navigate_to_monitor().await;
            }
        });
    }
}

async fn process_task(
    task: Task,
    backend: &BackendRuntime,
    keepalive_mode: String,
    image_markdown: bool,
) {
    let started = std::time::Instant::now();
    // 对齐 better-sqlite3 的数组绑定行为：input_images 存 JSON 字符串（空数组为 "[]"）
    let image_path_strings: Vec<String> = task
        .image_paths
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    let input_images = serde_json::to_string(&image_path_strings).unwrap_or_else(|_| "[]".into());
    if let Err(e) = history::create_record(&NewRecord {
        id: &task.id,
        model_id: Some(&task.model_id),
        model_name: Some(&task.model_name),
        prompt: &task.prompt,
        input_images: Some(input_images.as_str()),
        is_streaming: task.is_streaming,
    })
    .await
    {
        crate::logfmt::error("历史记录", &format!("创建请求记录失败: {e}"));
    }

    // 心跳：流式等待期间每 3 秒一次（queue.js:120-127）
    let heartbeat = if task.is_streaming {
        let sink = task.sink.clone();
        let mode = keepalive_mode.clone();
        let model = task.model_name.clone();
        Some(tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_millis(3000));
            ticker.tick().await;
            loop {
                ticker.tick().await;
                if sink.send(respond::heartbeat(&mode, Some(&model))).is_err() {
                    break;
                }
            }
        }))
    } else {
        None
    };

    let paths = image_path_strings;
    let result = backend
        .generate(
            &task.id,
            &task.prompt,
            &paths,
            &task.model_id,
            task.reasoning,
        )
        .await;

    if let Some(h) = heartbeat {
        h.abort();
    }

    let duration = started.elapsed().as_millis() as i64;
    let mut task = task;
    match result {
        Ok(gen) => finish_ok(&mut task, gen, image_markdown, duration).await,
        // 桥传输故障（断连/超时）按服务不可用处理，而非 500 内部错误
        Err(e) => {
            finish_err(
                &mut task,
                &e.to_string(),
                "SERVICE_UNAVAILABLE",
                true,
                503,
                duration,
            )
            .await
        }
    }
    for p in &task.image_paths {
        std::fs::remove_file(p).ok();
    }
}

async fn finish_ok(task: &mut Task, gen: GenerateResult, image_markdown: bool, duration: i64) {
    if let Some(error) = &gen.error {
        let status: u16 = if gen.retryable { 503 } else { 502 };
        finish_err(
            task,
            error,
            gen.code.as_deref().unwrap_or("GENERATION_FAILED"),
            gen.retryable,
            status,
            duration,
        )
        .await;
        return;
    }
    let image = if let Some(path) = &gen.image_path {
        match data_uri_of(path, gen.image_mime.as_deref().unwrap_or("image/png")).await {
            Ok(uri) => Some(uri),
            Err(error) => {
                finish_err(
                    task,
                    &format!("读取生成媒体失败: {error}"),
                    "GENERATION_FAILED",
                    false,
                    502,
                    duration,
                )
                .await;
                return;
            }
        }
    } else {
        gen.image_url.clone()
    };
    let (content, history_text, response_media) = if let Some(image) = image {
        let media = if image.starts_with("data:") {
            match history::save_data_uri(&image, &task.id).await {
                Ok(saved) => json!({
                    "type": saved["type"],
                    "originalUrl": gen.image_url.as_ref().filter(|url| !url.starts_with("data:")),
                    "localPath": saved["localPath"],
                    "status": "downloaded"
                }),
                Err(_) => {
                    json!({"type":"unknown","originalUrl":null,"localPath":null,"status":"failed"})
                }
            }
        } else {
            json!({"type":"unknown","originalUrl":image,"localPath":null,"status":"external"})
        };
        let history_text = image_history_text(gen.image_url.as_deref());
        let content = image_content(image, image_markdown);
        (content, history_text, json!([media]))
    } else {
        let text = gen.text.clone().unwrap_or_else(|| "生成失败".to_string());
        (text.clone(), text, json!([]))
    };
    let response_media_json = response_media.to_string();
    stats::increment_success().await;

    let _ = history::update_record(
        &task.id,
        &RecordUpdate {
            status: Some("success"),
            response_text: Some(&history_text),
            reasoning_content: gen.reasoning.as_deref(),
            response_media: Some(&response_media_json),
            error_message: None,
            duration_ms: Some(duration),
        },
    )
    .await;

    let reasoning = gen.reasoning.as_deref();
    if task.is_streaming {
        let chunk = respond::chat_completion_chunk(
            &content,
            Some(&task.model_name),
            Some("stop"),
            reasoning,
        );
        let _ = task.sink.send(respond::sse_event(&chunk));
        let _ = task.sink.send(respond::sse_done().to_string());
    } else if let Some(reply) = task.reply.take() {
        let body = respond::chat_completion(&content, Some(&task.model_name), reasoning);
        let _ = reply.send((200, body));
    }
}

async fn finish_err(
    task: &mut Task,
    message: &str,
    code: &str,
    _retryable: bool,
    status: u16,
    duration: i64,
) {
    stats::increment_failed().await;
    let _ = history::update_record(
        &task.id,
        &RecordUpdate {
            status: Some("failed"),
            response_text: None,
            reasoning_content: None,
            response_media: None,
            error_message: Some(message),
            duration_ms: Some(duration),
        },
    )
    .await;
    let body = respond::openai_error_body(code, Some(message));
    if task.is_streaming {
        let _ = task.sink.send(respond::sse_event(&body));
        let _ = task.sink.send(respond::sse_done().to_string());
    } else if let Some(reply) = task.reply.take() {
        let _ = reply.send((status, body));
    }
}
async fn data_uri_of(path: &str, mime: &str) -> std::io::Result<String> {
    let path = path.to_owned();
    let mime = mime.to_owned();
    tokio::task::spawn_blocking(move || {
        let bytes = std::fs::read(path)?;
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes);
        Ok(format!("data:{mime};base64,{b64}"))
    })
    .await
    .map_err(std::io::Error::other)?
}

fn image_history_text(original_url: Option<&str>) -> String {
    original_url
        .filter(|url| !url.starts_with("data:"))
        .unwrap_or_default()
        .to_owned()
}

fn image_content(image: String, markdown: bool) -> String {
    if markdown {
        format!("![generated]({image})")
    } else {
        image
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn image_output_does_not_store_base64_as_history_text() {
        let path = std::env::temp_dir().join(format!(
            "webai2api-image-output-{}-{}.png",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::write(&path, b"png bytes").unwrap();
        let data = data_uri_of(path.to_str().unwrap(), "image/png")
            .await
            .unwrap();
        std::fs::remove_file(path).unwrap();
        assert!(data.starts_with("data:image/png;base64,"));
        assert_eq!(image_history_text(Some(&data)), "");
        assert_eq!(
            image_content(data.clone(), true),
            format!("![generated]({data})")
        );
        assert_eq!(
            image_content(data, false),
            "data:image/png;base64,cG5nIGJ5dGVz"
        );
        assert_eq!(
            image_history_text(Some("https://example.test/a.png")),
            "https://example.test/a.png"
        );
    }

    /// 连接一个"只接受连接、不回应"的本地 socket，供 Queue 构造 Bridge。
    async fn dummy_bridge(tag: &str) -> Bridge {
        let sock =
            std::env::temp_dir().join(format!("webai2api-qtest-{tag}-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&sock);
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        tokio::spawn(async move {
            while let Ok((s, _)) = listener.accept().await {
                // Keep requests pending long enough to inspect the queue. An
                // immediate disconnect races the status assertion on CI.
                tokio::spawn(async move {
                    let _socket = s;
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                });
            }
        });
        Bridge::connect(sock, |_| {}).await.unwrap()
    }

    fn stream_task(id: &str) -> Task {
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        Task {
            id: id.to_string(),
            prompt: "p".into(),
            image_paths: vec![],
            model_id: "m".into(),
            model_name: "m".into(),
            is_streaming: true,
            reasoning: false,
            sink: tx,
            reply: None,
            enqueued_at: std::time::Instant::now(),
        }
    }

    #[test]
    fn reserve_cancel_symmetry() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let bridge = dummy_bridge("sym").await;
            // maxConcurrent=2 + queueBuffer=1 → 上限 3
            let q = Queue::new(bridge, 2, 1, "comment".into(), false);
            assert!(q.try_reserve());
            assert!(q.try_reserve());
            assert!(q.try_reserve());
            assert!(!q.try_reserve(), "超过 maxConcurrent+queueBuffer 必须拒绝");
            q.cancel_reservation();
            assert!(q.try_reserve(), "释放一个名额后应可再次准入");
            q.cancel_reservation();
            q.cancel_reservation();
            q.cancel_reservation();
            q.cancel_reservation();
            q.cancel_reservation();
            // 原子回绕防御：多取消不应把 reserved 打成负数（fetch_sub 饱和到 0 的语义由调用方保证，
            // 这里只验证 5 次取消 + 3 次成功后仍能重新准入）
            assert!(q.try_reserve());
        });
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_try_reserve_never_exceeds_limit() {
        let bridge = dummy_bridge("race").await;
        let q = Arc::new(Queue::new(bridge, 2, 2, "comment".into(), false)); // 上限 4
        for _round in 0..50 {
            let handles: Vec<_> = (0..12)
                .map(|_| {
                    let q = Arc::clone(&q);
                    tokio::spawn(async move { q.try_reserve() })
                })
                .collect();
            let mut granted = 0usize;
            for h in handles {
                if h.await.unwrap() {
                    granted += 1;
                }
            }
            assert!(granted <= 4, "并发准入突破上限: granted={granted} > 4");
            for _ in 0..granted {
                q.cancel_reservation();
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn enqueue_updates_status() {
        let bridge = dummy_bridge("status").await;
        let q = Arc::new(Queue::new(bridge, 2, 0, "comment".into(), false));
        assert_eq!(q.status_total(), 0);
        q.enqueue(stream_task("t1"));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(q.status_total(), 1, "任务应已进入 waiting/processing");
        let status = q.detailed_status();
        assert_eq!(status["total"], 1);
        let listed = status["processingTasks"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .len()
            + status["waitingTasks"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .len();
        assert_eq!(listed, 1, "processing+waiting 快照数应等于 total");
    }
}
