//! 端到端集成测试：在进程内用 mock 引擎桥驱动完整服务（run::supervise），
//! 覆盖 HTTP/队列/安全模式/桥崩溃自愈/优雅停机链路。不需要浏览器内核。
//!
//! 注意：history/stats/logfmt 是进程级全局状态，测试之间用 SERIAL 互斥串行执行。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::Mutex;

use webai2api_rs::run::{self, StartOptions};

/// 进程级全局状态（history/stats/logfmt）的串行化锁。
static SERIAL: Mutex<()> = Mutex::const_new(());

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("webai2api-it-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn mock_bridge_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("bridge/mock-bridge.mjs")
}

fn write_config(dir: &Path, body: &str) {
    std::fs::create_dir_all(dir.join("data")).unwrap();
    std::fs::write(dir.join("data/config.yaml"), body).unwrap();
}

fn single_worker_config(port: u16, tag: &str) -> String {
    format!(
        r#"
server: {{ port: {port}, auth: "" }}
backend:
  pool:
    instances:
      - {{ name: main{tag}, workers: [{{ name: w{tag}, type: mock }}] }}
"#
    )
}

fn base_opts(cfg: &Path, data: &Path) -> StartOptions {
    StartOptions {
        src_root: cfg.to_path_buf(),
        data_dir: data.to_path_buf(),
        login: None,
        xvfb: false,
        vnc: false,
        bridge_script: Some(mock_bridge_path()),
        extra_envs: Vec::new(),
    }
}

fn worker_tag(port: u16) -> String {
    format!("i{}", port % 1000)
}

async fn get(port: u16, path: &str) -> (u16, Value) {
    let resp = reqwest::get(format!("http://127.0.0.1:{port}{path}"))
        .await
        .unwrap_or_else(|e| panic!("GET {path} 失败: {e}"));
    let status = resp.status().as_u16();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    (status, body)
}

async fn post(port: u16, path: &str, body: Value) -> (u16, Value) {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}{path}"))
        .json(&body)
        .send()
        .await
        .unwrap_or_else(|e| panic!("POST {path} 失败: {e}"));
    let status = resp.status().as_u16();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    (status, body)
}

/// 轮询直到 /ready 的 JSON 满足谓词。
async fn wait_until(port: u16, pred: impl Fn(&Value) -> bool, what: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(resp) = reqwest::get(format!("http://127.0.0.1:{port}/ready")).await {
            if let Ok(v) = resp.json::<Value>().await {
                if pred(&v) {
                    return v;
                }
            }
        }
        assert!(Instant::now() < deadline, "超时：{what}");
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

async fn wait_ready(port: u16) {
    wait_until(port, |v| v["ready"] == true, "服务就绪").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn endpoints_generation_and_graceful_stop() {
    let _serial = SERIAL.lock().await;
    let cfg = temp_dir("cfg1");
    let data = temp_dir("data1");
    let port = free_port();
    write_config(&cfg, &single_worker_config(port, &worker_tag(port)));

    let handle = run::supervise(base_opts(&cfg, &data)).await;
    wait_ready(port).await;

    // 模型与诊断端点
    let (status, models) = get(port, "/v1/models").await;
    assert_eq!(status, 200);
    let ids: Vec<&str> = models["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"gpt-test"), "{models}");

    // 文本生成（非流式）
    let (status, body) = post(
        port,
        "/v1/chat/completions",
        serde_json::json!({"model": "gpt-test", "messages": [{"role": "user", "content": "你好"}]}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body.pointer("/choices/0/message/content")
            .and_then(Value::as_str),
        Some("echo:你好")
    );

    // reasoning 透传
    let (_, body) = post(port, "/v1/chat/completions",
        serde_json::json!({"model": "gpt-test", "messages": [{"role": "user", "content": "hi"}], "reasoning": true})).await;
    assert_eq!(
        body.pointer("/choices/0/message/reasoning_content")
            .and_then(Value::as_str),
        Some("mock-reasoning")
    );

    // 适配器错误 → 502 + 透传 code
    let (status, body) = post(port, "/v1/chat/completions",
        serde_json::json!({"model": "gpt-test", "messages": [{"role": "user", "content": "失败一下"}]})).await;
    assert_eq!(status, 502, "{body}");
    assert_eq!(body["error"]["code"], "CONTENT_BLOCKED");

    // stateless 拒绝 conversation_id；未知模型 400
    let (status, _) = post(port, "/v1/stateless/chat/completions",
        serde_json::json!({"model": "gpt-test", "conversation_id": "c1", "messages": [{"role": "user", "content": "x"}]})).await;
    assert_eq!(status, 400);
    let (status, body) = post(
        port,
        "/v1/chat/completions",
        serde_json::json!({"model": "nope", "messages": [{"role": "user", "content": "x"}]}),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"]["code"], "INVALID_MODEL");

    // 流式 SSE：心跳/数据行 + [DONE] 结束
    let client = reqwest::Client::new();
    let resp = client.post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .json(&serde_json::json!({"model": "gpt-test", "stream": true, "messages": [{"role": "user", "content": "slow流"}]}))
        .send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert!(resp.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/event-stream"));
    let text = resp.text().await.unwrap();
    assert!(
        text.contains(": keepalive") || text.contains("data: "),
        "应有心跳或数据行: {text}"
    );
    assert!(
        text.trim_end().ends_with("[DONE]"),
        "应以 [DONE] 结束: {text}"
    );

    // 历史记录已落库
    let (status, hist) = get(port, "/admin/history?page=1&pageSize=10").await;
    assert_eq!(status, 200);
    assert!(
        hist["total"].as_u64().unwrap() >= 3,
        "应有成功+失败记录: {hist}"
    );

    // 配置写回校验（非法 400 + 合法 200 落盘）
    let (status, body) = post(
        port,
        "/admin/config/server",
        serde_json::json!({"port": 99999}),
    )
    .await;
    assert_eq!(status, 400);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("配置校验失败"),
        "{body}"
    );
    let (status, body) = post(
        port,
        "/admin/config/server",
        serde_json::json!({"queueBuffer": 4}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let yaml = std::fs::read_to_string(cfg.join("data/config.yaml")).unwrap();
    assert!(yaml.contains("queueBuffer: 4"), "配置应已写回: {yaml}");

    // 停机：锁文件清理
    handle.shutdown().await;
    assert!(
        !data.join(".webai2api-supervisor.lock").exists(),
        "锁文件应已清理"
    );
    let _ = std::fs::remove_dir_all(&cfg);
    let _ = std::fs::remove_dir_all(&data);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn safe_mode_returns_503() {
    let _serial = SERIAL.lock().await;
    let cfg = temp_dir("cfg2");
    let data = temp_dir("data2");
    let port = free_port();
    write_config(&cfg, &single_worker_config(port, &worker_tag(port)));

    let mut opts = base_opts(&cfg, &data);
    opts.extra_envs = vec![("MOCK_INIT_FAIL".to_string(), "1".to_string())];
    let handle = run::supervise(opts).await;

    wait_until(port, |v| v["safeMode"] == true, "进入安全模式").await;
    let (_, v) = get(port, "/ready").await;
    assert_eq!(v["poolReady"], false);
    assert!(v["safeModeReason"]
        .as_str()
        .unwrap()
        .contains("模拟初始化失败"));

    // 诊断端点放行，生成端点 503
    let (status, _) = get(port, "/v1/models").await;
    assert_eq!(status, 200);
    let (status, body) = post(
        port,
        "/v1/chat/completions",
        serde_json::json!({"model": "gpt-test", "messages": [{"role": "user", "content": "hi"}]}),
    )
    .await;
    assert_eq!(status, 503, "{body}");
    assert_eq!(body["error"]["code"], "SERVICE_UNAVAILABLE");

    // WebUI/Admin 仍可用
    let (status, _) = get(port, "/admin/status").await;
    assert_eq!(status, 200);

    handle.shutdown().await;
    let _ = std::fs::remove_dir_all(&cfg);
    let _ = std::fs::remove_dir_all(&data);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_crash_triggers_auto_restart() {
    let _serial = SERIAL.lock().await;
    let cfg = temp_dir("cfg3");
    let data = temp_dir("data3");
    let port = free_port();
    write_config(&cfg, &single_worker_config(port, &worker_tag(port)));

    let handle = run::supervise(base_opts(&cfg, &data)).await;
    wait_ready(port).await;

    // 触发桥崩溃：响应可能是 503 或连接重置（重启竞争），两者都接受
    let client = reqwest::Client::new();
    let r = client.post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .json(&serde_json::json!({"model": "gpt-test", "messages": [{"role": "user", "content": "crash-bridge"}]}))
        .timeout(Duration::from_secs(30))
        .send().await;
    if let Ok(resp) = r {
        assert_eq!(resp.status().as_u16(), 503, "桥崩溃请求应 503");
    }

    // 看门狗自动重启后恢复正常服务
    wait_until(port, |v| v["ready"] == true, "看门狗恢复服务").await;
    let (status, body) = post(
        port,
        "/v1/chat/completions",
        serde_json::json!({"model": "gpt-test", "messages": [{"role": "user", "content": "回来"}]}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body.pointer("/choices/0/message/content")
            .and_then(Value::as_str),
        Some("echo:回来")
    );

    handle.shutdown().await;
    let _ = std::fs::remove_dir_all(&cfg);
    let _ = std::fs::remove_dir_all(&data);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queue_full_rejects_non_streaming_with_429() {
    let _serial = SERIAL.lock().await;
    let cfg = temp_dir("cfg4");
    let data = temp_dir("data4");
    let port = free_port();
    // queueBuffer=2、maxConcurrent=1（1 个 worker）→ 非流式上限 3
    write_config(
        &cfg,
        &format!(
            r#"
server: {{ port: {port}, auth: "" }}
queue: {{ queueBuffer: 2 }}
backend:
  pool:
    instances:
      - {{ name: main4, workers: [{{ name: w4, type: mock }}] }}
"#
        ),
    );

    let handle = run::supervise(base_opts(&cfg, &data)).await;
    wait_ready(port).await;

    let client = reqwest::Client::new();
    let mut handles = Vec::new();
    for i in 0..5 {
        let client = client.clone();
        let url = format!("http://127.0.0.1:{port}/v1/chat/completions");
        handles.push(tokio::spawn(async move {
            let resp = client
                .post(&url)
                .json(&serde_json::json!({"model": "gpt-test",
                    "messages": [{"role": "user", "content": format!("slow {i}")}]}))
                .timeout(Duration::from_secs(30))
                .send()
                .await;
            match resp {
                Ok(r) => {
                    let status = r.status().as_u16();
                    let body = r.json::<Value>().await.unwrap_or(Value::Null);
                    (status, body)
                }
                Err(_) => (0u16, Value::Null),
            }
        }));
    }
    let mut ok = 0;
    let mut busy = 0;
    for h in handles {
        let (status, body) = h.await.unwrap();
        match status {
            200 => ok += 1,
            429 => {
                busy += 1;
                assert_eq!(body["error"]["code"], "SERVER_BUSY");
                assert!(
                    body["error"]["message"]
                        .as_str()
                        .unwrap()
                        .contains("服务器繁忙"),
                    "{body}"
                );
            }
            other => panic!("意外状态 {other}: {body}"),
        }
    }
    assert!(busy >= 1, "至少一个请求应被 429 拒绝");
    assert_eq!(ok + busy, 5);
    // 全部完成后队列清空
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (_, q) = get(port, "/admin/queue").await;
    assert_eq!(q["total"], 0, "{q}");

    handle.shutdown().await;
    let _ = std::fs::remove_dir_all(&cfg);
    let _ = std::fs::remove_dir_all(&data);
}

fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// flock 跨进程互斥：任一进程持锁时，另一进程启动必须被拒绝（互斥语义不能只靠 PID 存活检测）。
#[test]
fn lock_cross_process() {
    let dir = std::env::temp_dir().join(format!("webai2api-lockproc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // 测试进程自身持锁
    webai2api_rs::instance_lock::acquire_lock(&dir).expect("测试进程应能获取锁");

    let binary = env!("CARGO_BIN_EXE_webai2api");
    let out = std::process::Command::new(binary)
        .arg("--lock-probe")
        .arg(&dir)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "LOCKED-ELSEWHERE"
    );
    assert_eq!(out.status.code(), Some(0));

    webai2api_rs::instance_lock::release_lock(&dir);
    let out = std::process::Command::new(binary)
        .arg("--lock-probe")
        .arg(&dir)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "ACQUIRED");

    let _ = std::fs::remove_dir_all(&dir);
}

// Arc 引用避免 unused 警告（AppState 相关测试未来扩展用）
#[allow(unused)]
fn _assert_send() {
    fn is_send<T: Send>() {}
    is_send::<Arc<run::StartOptions>>();
}
