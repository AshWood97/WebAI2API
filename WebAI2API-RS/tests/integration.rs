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
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("config.yaml"), body).unwrap();
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn generic_browser_rpc_runs_rust_adapter_through_http() {
    let _serial = SERIAL.lock().await;
    let cfg = temp_dir("rust-runtime-cfg");
    let data = temp_dir("rust-runtime-data");
    let port = free_port();
    write_config(
        &data,
        &format!(
            "server: {{ port: {port}, auth: \"\" }}\nbackend:\n  pool:\n    instances:\n      - {{ name: main, workers: [{{ name: rust-test, type: test }}] }}\n"
        ),
    );
    let mut opts = base_opts(&cfg, &data);
    opts.bridge_script = None;
    opts.extra_envs = vec![(
        "WEBAI2API_BROWSER_RUNTIME_SCRIPT".into(),
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("bridge/mock-browser-runtime.mjs")
            .to_string_lossy()
            .into_owned(),
    )];
    let handle = run::supervise(opts).await;
    wait_ready(port).await;

    let (status, models) = get(port, "/v1/models").await;
    assert_eq!(status, 200, "{models}");
    assert!(
        models["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|model| model["id"] == "ip"),
        "{models}"
    );
    let (status, body) = post(
        port,
        "/v1/chat/completions",
        serde_json::json!({"model":"ip", "messages":[{"role":"user", "content":"check"}]}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body.pointer("/choices/0/message/content")
            .and_then(Value::as_str),
        Some("203.0.113.42")
    );

    handle.shutdown().await;
    let _ = std::fs::remove_dir_all(&cfg);
    let _ = std::fs::remove_dir_all(&data);
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

async fn get_with_auth(port: u16, path: &str, token: &str) -> (u16, Value) {
    let resp = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}{path}"))
        .bearer_auth(token)
        .send()
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

async fn post_with_auth(port: u16, path: &str, token: &str, body: Value) -> (u16, Value) {
    let resp = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}{path}"))
        .bearer_auth(token)
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
    write_config(&data, &single_worker_config(port, &worker_tag(port)));

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

    // 适配器错误 → 与 Node 队列一致，HTTP 层统一 GENERATION_FAILED。
    let (status, body) = post(port, "/v1/chat/completions",
        serde_json::json!({"model": "gpt-test", "messages": [{"role": "user", "content": "失败一下"}]})).await;
    assert_eq!(status, 502, "{body}");
    assert_eq!(body["error"]["code"], "GENERATION_FAILED");

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

    // 日期范围删除也必须删除 history/media 中由记录引用的文件。
    let media_record_id = format!("media-delete-{port}");
    webai2api_rs::history::create_record(&webai2api_rs::history::NewRecord {
        id: &media_record_id,
        model_id: Some("gpt-test"),
        model_name: Some("gpt-test"),
        prompt: "media cleanup regression",
        input_images: None,
        is_streaming: false,
    })
    .await
    .unwrap();
    let media =
        webai2api_rs::history::save_data_uri("data:image/png;base64,aGVsbG8=", &media_record_id)
            .await
            .unwrap();
    let media_path = PathBuf::from(media["localPath"].as_str().unwrap());
    let media_json = serde_json::json!([media]).to_string();
    webai2api_rs::history::update_record(
        &media_record_id,
        &webai2api_rs::history::RecordUpdate {
            status: Some("success"),
            response_text: None,
            reasoning_content: None,
            response_media: Some(&media_json),
            error_message: None,
            duration_ms: None,
        },
    )
    .await
    .unwrap();
    let today = chrono::Local::now().format("%Y-%m-%d");
    let resp = reqwest::Client::new()
        .delete(format!(
            "http://127.0.0.1:{port}/admin/history?startDate={today}&endDate={today}"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert!(
        !media_path.exists(),
        "日期删除遗留了媒体文件: {}",
        media_path.display()
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
    let yaml = std::fs::read_to_string(data.join("config.yaml")).unwrap();
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
    write_config(&data, &single_worker_config(port, &worker_tag(port)));

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
    let webui = reqwest::get(format!("http://127.0.0.1:{port}/"))
        .await
        .unwrap();
    assert_eq!(
        webui.status().as_u16(),
        200,
        "WebUI index must resolve in a checkout"
    );
    assert!(
        webui
            .text()
            .await
            .unwrap()
            .to_ascii_lowercase()
            .contains("<html"),
        "WebUI index response was not the bundled page"
    );

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
    write_config(&data, &single_worker_config(port, &worker_tag(port)));

    let mut opts = base_opts(&cfg, &data);
    let descendant_marker = data.join("bridge-descendant-survived");
    opts.extra_envs.push((
        "MOCK_DESCENDANT_MARKER".to_string(),
        descendant_marker.to_string_lossy().into_owned(),
    ));
    let handle = run::supervise(opts).await;
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

    // If the bridge process group was not reaped, its long-lived child writes this marker.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(
        !descendant_marker.exists(),
        "bridge crash left a descendant process running (marker: {})",
        descendant_marker.display()
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
        &data,
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

/// Child entry point used by startup_sigterm_cleans_bridge_and_lock. It intentionally blocks
/// inside mock bridge initialization so SIGTERM exercises startup cleanup, before HTTP starts.
#[tokio::test(flavor = "current_thread")]
async fn startup_sigterm_child_entry() {
    if std::env::var_os("WEBAI2API_SIGTERM_CHILD").is_none() {
        return;
    }
    let cfg = PathBuf::from(std::env::var_os("WEBAI2API_SIGTERM_CFG").unwrap());
    let data = PathBuf::from(std::env::var_os("WEBAI2API_SIGTERM_DATA").unwrap());
    let started_file = PathBuf::from(std::env::var_os("WEBAI2API_SIGTERM_STARTED").unwrap());
    let opts = StartOptions {
        src_root: cfg,
        data_dir: data.clone(),
        bridge_script: Some(mock_bridge_path()),
        extra_envs: vec![
            ("MOCK_INIT_DELAY_MS".into(), "10000".into()),
            (
                "MOCK_INIT_STARTED_FILE".into(),
                started_file.to_string_lossy().into_owned(),
            ),
        ],
        ..StartOptions::default()
    };
    webai2api_rs::instance_lock::acquire_lock(&data).unwrap();
    run::watchdog(opts).await;
}

#[cfg(unix)]
#[test]
fn startup_sigterm_cleans_bridge_and_lock() {
    use std::process::{Command, Stdio};

    let cfg = temp_dir("sigterm-cfg");
    let data = temp_dir("sigterm-data");
    let started = data.join("init-started");
    let port = free_port();
    write_config(&data, &single_worker_config(port, &worker_tag(port)));
    let exe = std::env::current_exe().unwrap();
    let mut child = Command::new(exe)
        .args(["--exact", "startup_sigterm_child_entry", "--nocapture"])
        .env("WEBAI2API_SIGTERM_CHILD", "1")
        .env("WEBAI2API_SIGTERM_CFG", &cfg)
        .env("WEBAI2API_SIGTERM_DATA", &data)
        .env("WEBAI2API_SIGTERM_STARTED", &started)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(20);
    while !started.exists() {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("child exited before bridge init started: {status}");
        }
        assert!(
            Instant::now() < deadline,
            "mock bridge never entered delayed init"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "SIGTERM child did not finish cleanup"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "SIGTERM child exit: {status}");
    assert!(
        !data.join(".webai2api-supervisor.lock").exists(),
        "startup SIGTERM left the instance lock behind"
    );
    let bridge_pid: i32 = std::fs::read_to_string(&started).unwrap().parse().unwrap();
    let bridge_gone_by = Instant::now() + Duration::from_secs(3);
    while unsafe { libc::kill(bridge_pid, 0) } == 0 {
        assert!(
            Instant::now() < bridge_gone_by,
            "startup SIGTERM left bridge process {bridge_pid}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let socket = std::env::temp_dir().join(format!("w2a-{}.sock", child.id()));
    if socket.exists() {
        // A stale Unix socket file cannot accept connections, but should not leak from this test.
        let _ = std::fs::remove_file(&socket);
    }
    let _ = std::fs::remove_dir_all(&cfg);
    let _ = std::fs::remove_dir_all(&data);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_config_patches_preserve_both_updates() {
    let _serial = SERIAL.lock().await;
    let cfg = temp_dir("cfg-concurrent");
    let data = temp_dir("data-concurrent");
    let port = free_port();
    write_config(&data, &single_worker_config(port, &worker_tag(port)));
    let handle = run::supervise(base_opts(&cfg, &data)).await;
    wait_ready(port).await;

    let (server_result, pool_result) = tokio::join!(
        post(
            port,
            "/admin/config/server",
            serde_json::json!({"queueBuffer": 19}),
        ),
        post(
            port,
            "/admin/config/pool",
            serde_json::json!({"strategy": "round_robin"}),
        )
    );
    assert_eq!(server_result.0, 200, "{}", server_result.1);
    assert_eq!(pool_result.0, 200, "{}", pool_result.1);
    let persisted: serde_yaml::Value =
        serde_yaml::from_str(&std::fs::read_to_string(data.join("config.yaml")).unwrap()).unwrap();
    assert_eq!(persisted["queue"]["queueBuffer"].as_u64(), Some(19));
    assert_eq!(
        persisted["backend"]["pool"]["strategy"].as_str(),
        Some("round_robin")
    );

    handle.shutdown().await;
    let _ = std::fs::remove_dir_all(&cfg);
    let _ = std::fs::remove_dir_all(&data);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn saved_config_get_reflects_disk_without_reloading_runtime() {
    let _serial = SERIAL.lock().await;
    let cfg = temp_dir("cfg-get-saved");
    let data = temp_dir("data-get-saved");
    let port = free_port();
    let initial_config = format!(
        concat!(
            "server:\n",
            "  port: {}\n",
            "  auth: \"\"\n",
            "browser:\n",
            "  engine: camoufox\n",
            "  clearcote:\n",
            "    acceptLanguage: \"en-US,en\"\n",
            "backend:\n",
            "  pool:\n",
            "    strategy: least_busy\n",
            "    instances:\n",
            "      - name: main\n",
            "        workers:\n",
            "          - name: w\n",
            "            type: mock\n",
        ),
        port
    );
    write_config(&data, &initial_config);

    let handle = run::supervise(base_opts(&cfg, &data)).await;
    wait_ready(port).await;

    let (status, browser_save) = post(
        port,
        "/admin/config/browser",
        serde_json::json!({
            "engine": "clearcote",
            "clearcote": { "acceptLanguage": "zh-CN,zh" }
        }),
    )
    .await;
    assert_eq!(status, 200, "{browser_save}");
    let (status, server_save) = post(
        port,
        "/admin/config/server",
        serde_json::json!({"queueBuffer": 9}),
    )
    .await;
    assert_eq!(status, 200, "{server_save}");
    let (status, instances_save) = post(
        port,
        "/admin/config/instances",
        serde_json::json!([{
            "name": "saved-instance",
            "engine": "clearcote",
            "workers": [{"name": "saved-worker", "type": "lmarena"}]
        }]),
    )
    .await;
    assert_eq!(status, 200, "{instances_save}");
    let (status, adapters_save) = post(
        port,
        "/admin/config/adapters",
        serde_json::json!({"saved-adapter": {"custom": "persisted"}}),
    )
    .await;
    assert_eq!(status, 200, "{adapters_save}");
    let (status, pool_save) = post(
        port,
        "/admin/config/pool",
        serde_json::json!({"strategy": "round_robin"}),
    )
    .await;
    assert_eq!(status, 200, "{pool_save}");

    let (status, server) = get(port, "/admin/config/server").await;
    assert_eq!(status, 200, "{server}");
    assert_eq!(server["queueBuffer"], 9);

    let (status, browser) = get(port, "/admin/config/browser").await;
    assert_eq!(status, 200, "{browser}");
    assert_eq!(browser["engine"], "clearcote");
    assert_eq!(browser["clearcote"]["acceptLanguage"], "zh-CN,zh");

    let (status, instances) = get(port, "/admin/config/instances").await;
    assert_eq!(status, 200, "{instances}");
    assert_eq!(instances[0]["name"], "saved-instance");

    let (status, workers) = get(port, "/admin/config/workers").await;
    assert_eq!(status, 200, "{workers}");
    assert_eq!(workers[0]["workers"][0]["name"], "saved-worker");

    let (status, adapters) = get(port, "/admin/config/adapters").await;
    assert_eq!(status, 200, "{adapters}");
    assert_eq!(adapters["saved-adapter"]["custom"], "persisted");

    let (status, pool) = get(port, "/admin/config/pool").await;
    assert_eq!(status, 200, "{pool}");
    assert_eq!(pool["strategy"], "round_robin");

    let (status, runtime) = get(port, "/v1/runtime/status").await;
    assert_eq!(status, 200, "{runtime}");
    assert_eq!(runtime["browser"]["defaultEngine"], "camoufox");
    assert_eq!(runtime["pool"]["strategy"], "least_busy");
    assert_eq!(runtime["pool"]["workerCount"], 1);
    assert!(runtime["browser"]["workers"][0]["pageReady"].is_boolean());
    assert_ne!(runtime["browser"]["workers"][0]["name"], "saved-worker");

    handle.shutdown().await;
    let _ = std::fs::remove_dir_all(&cfg);
    let _ = std::fs::remove_dir_all(&data);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_auth_token_is_hidden_until_restart_then_rotates() {
    let _serial = SERIAL.lock().await;
    let cfg = temp_dir("cfg-auth-pending");
    let data = temp_dir("data-auth-pending");
    let port = free_port();
    let initial_config = single_worker_config(port, &worker_tag(port))
        .replace("auth: \"\"", "auth: active-old-token");
    write_config(&data, &initial_config);

    let opts = base_opts(&cfg, &data);
    let handle = run::supervise(opts.clone()).await;
    wait_ready(port).await;

    let (status, saved) = post_with_auth(
        port,
        "/admin/config/server",
        "active-old-token",
        serde_json::json!({"authToken": "pending-new-token"}),
    )
    .await;
    assert_eq!(status, 200, "{saved}");

    let (status, server) = get_with_auth(port, "/admin/config/server", "active-old-token").await;
    assert_eq!(status, 200, "{server}");
    assert_eq!(server["authToken"], "active-old-token");
    assert_eq!(server["authTokenPendingRestart"], true);
    assert!(!server.to_string().contains("pending-new-token"));
    let (status, _) = get_with_auth(port, "/admin/config/server", "pending-new-token").await;
    assert_eq!(status, 401);

    handle.shutdown().await;
    let restarted = run::supervise(opts).await;
    wait_ready(port).await;
    let (status, _) = get_with_auth(port, "/admin/config/server", "active-old-token").await;
    assert_eq!(status, 401);
    let (status, server) = get_with_auth(port, "/admin/config/server", "pending-new-token").await;
    assert_eq!(status, 200, "{server}");
    assert_eq!(server["authToken"], "pending-new-token");
    assert_eq!(server["authTokenPendingRestart"], false);

    restarted.shutdown().await;
    let _ = std::fs::remove_dir_all(&cfg);
    let _ = std::fs::remove_dir_all(&data);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_auth_config_does_not_start_open_server() {
    let _serial = SERIAL.lock().await;
    let cfg = temp_dir("cfg-bad-auth");
    let data = temp_dir("data-bad-auth");
    let port = free_port();
    write_config(
        &data,
        &format!("server: {{ port: {port}, auth: 1234567890 }}\nbackend: {{ pool: {{ instances: [{{ name: main, workers: [{{ name: w, type: mock }}] }}] }} }}\n"),
    );
    let handle = run::supervise(base_opts(&cfg, &data)).await;
    // Give the watcher enough time to either reject the malformed file or (on regression)
    // reach its HTTP listener. A bind probe is deterministic even if startup fails silently.
    let rejection_window = Instant::now() + Duration::from_millis(500);
    while Instant::now() < rejection_window {
        match tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
            Ok(_) => panic!("invalid auth config unexpectedly exposed an HTTP listener"),
            Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
    handle.shutdown().await;
    let _ = std::fs::remove_dir_all(&cfg);
    let _ = std::fs::remove_dir_all(&data);
}

/// flock 跨进程互斥：任一进程持锁时，另一进程启动必须被拒绝（互斥语义不能只靠 PID 存活检测）。
///
/// 探针子进程用 fork + execv 直接执行 cargo 注入的测试产物：无 shell 参与，
/// 参数只有 `--lock-probe` 和本测试自建的临时目录（不来自任何外部输入）。
#[test]
fn lock_cross_process() {
    let dir = std::env::temp_dir().join(format!("webai2api-lockproc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // 测试进程自身持锁
    webai2api_rs::instance_lock::acquire_lock(&dir).expect("测试进程应能获取锁");

    // fork 子进程执行探针，父进程从管道读其 stdout
    let probe = |dir: &std::path::Path| -> String {
        use std::ffi::CString;
        use std::io::Read;
        use std::os::unix::io::FromRawFd;
        let mut fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork 失败");
        if pid == 0 {
            unsafe {
                libc::close(fds[0]);
                libc::dup2(fds[1], 1);
                libc::close(fds[1]);
                let bin = CString::new(env!("CARGO_BIN_EXE_webai2api")).unwrap();
                let a1 = CString::new("--lock-probe").unwrap();
                let a2 = CString::new(dir.to_string_lossy().to_string()).unwrap();
                let argv: Vec<*const libc::c_char> =
                    vec![bin.as_ptr(), a1.as_ptr(), a2.as_ptr(), std::ptr::null()];
                libc::execv(bin.as_ptr(), argv.as_ptr());
                libc::_exit(127);
            }
        }
        unsafe { libc::close(fds[1]) };
        let mut file = unsafe { std::fs::File::from_raw_fd(fds[0]) };
        let mut out = String::new();
        let _ = file.read_to_string(&mut out);
        std::mem::forget(file);
        let mut status = 0i32;
        unsafe { libc::waitpid(pid, &mut status, 0) };
        assert!(libc::WIFEXITED(status), "探针进程异常终止");
        out.trim().to_string()
    };
    assert_eq!(probe(&dir), "LOCKED-ELSEWHERE");

    webai2api_rs::instance_lock::release_lock(&dir);
    assert_eq!(probe(&dir), "ACQUIRED");

    let _ = std::fs::remove_dir_all(&dir);
}

// Arc 引用避免 unused 警告（AppState 相关测试未来扩展用）
#[allow(unused)]
fn _assert_send() {
    fn is_send<T: Send>() {}
    is_send::<Arc<run::StartOptions>>();
}
