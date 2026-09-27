//! 服务器编排：看门狗重启循环、单轮服务（配置/桥/HTTP/IPC）、Xvfb/VNC 助手进程管理。
//! main.rs 只保留参数解析与锁；本模块同时被二进制（watchdog）与集成测试（supervise）使用。

use crate::bridge;
use crate::config::{self, ConfigError};
use crate::instance_lock::release_lock;
use crate::logfmt;
use crate::runtime::{self, BackendRuntime, RustRuntime};
use crate::server::{self, AppState, VncInfo};
use serde_json::json;
use std::future::IntoFuture;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

pub const FATAL_EXIT: i32 = 78;
pub const MAX_AUTO_RESTARTS: u32 = 3;

/// 一次服务运行的启动参数。
#[derive(Clone, Default)]
pub struct StartOptions {
    pub src_root: PathBuf,
    pub data_dir: PathBuf,
    pub login: Option<String>,
    pub xvfb: bool,
    pub vnc: bool,
    /// 覆盖引擎桥脚本（测试注入 mock 桥）；None 用安装目录的 bridge/bridge.mjs。
    pub bridge_script: Option<PathBuf>,
    /// 额外传给引擎桥子进程的环境变量（作用域限定在桥进程，避免污染本进程全局环境）。
    pub extra_envs: Vec<(String, String)>,
}

pub enum RunError {
    Fatal(String),
    Retryable(String),
    RestartWith(Vec<String>),
}

/// Xvfb/x11vnc 意外退出的事件。
pub enum HelperEvent {
    XvfbDead,
    VncDead,
}

/// 助手子进程（Xvfb、x11vnc）的持有者：跨重启轮存活，最终退出时统一回收。
#[derive(Default)]
pub struct Helpers {
    xvfb_pid: Option<u32>,
    vnc_pid: Option<u32>,
    vnc_info: VncInfo,
}

impl Helpers {
    /// 按需拉起 Xvfb / x11vnc。Xvfb 意外退出后环境变量被清除，下一轮会在这里重新拉起。
    async fn ensure(
        &mut self,
        opts: &StartOptions,
        events: &mpsc::UnboundedSender<HelperEvent>,
    ) -> Result<(), String> {
        if opts.xvfb
            && cfg!(target_os = "linux")
            && std::env::var("XVFB_RUNNING").is_err()
            && self.xvfb_pid.is_none()
        {
            let (display, child) = launch_xvfb().await?;
            std::env::set_var("DISPLAY", &display);
            std::env::set_var("XVFB_RUNNING", "true");
            self.xvfb_pid = Some(child.id().unwrap_or(0));
            logfmt::info("看门狗", &format!("Xvfb 已启动 (显示号: {display})"));
            tokio::spawn(watch_helper(child, events.clone(), HelperEvent::XvfbDead));
        }
        if opts.vnc && std::env::var("XVFB_RUNNING").is_ok() && self.vnc_pid.is_none() {
            match launch_vnc().await {
                Ok((port, child)) => {
                    let display = std::env::var("DISPLAY").unwrap_or_default();
                    logfmt::info("看门狗", &format!("VNC 服务器已启动，端口: {port}"));
                    self.vnc_pid = Some(child.id().unwrap_or(0));
                    self.vnc_info = VncInfo {
                        enabled: true,
                        port,
                        display,
                        xvfb_mode: true,
                    };
                    tokio::spawn(watch_helper(child, events.clone(), HelperEvent::VncDead));
                }
                Err(e) => {
                    logfmt::warn("看门狗", &e);
                    self.vnc_info = VncInfo {
                        xvfb_mode: true,
                        ..VncInfo::default()
                    };
                }
            }
        }
        Ok(())
    }

    /// 终止全部助手进程（服务最终退出时调用；重启轮不调用，Xvfb/VNC 跨轮存活）。
    async fn terminate(&self) {
        self.terminate_vnc().await;
        if let Some(pid) = self.xvfb_pid {
            terminate_pid(pid).await;
        }
    }

    async fn terminate_vnc(&self) {
        if let Some(pid) = self.vnc_pid {
            terminate_pid(pid).await;
        }
    }
}

async fn watch_helper(
    mut child: tokio::process::Child,
    events: mpsc::UnboundedSender<HelperEvent>,
    event: HelperEvent,
) {
    let _ = child.wait().await;
    let _ = events.send(event);
}

/// SIGTERM → 宽限 300ms → SIGKILL。pid=0 防御：kill(0) 会波及调用者整个进程组。
async fn terminate_pid(pid: u32) {
    if pid == 0 {
        return;
    }
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
    #[cfg(not(unix))]
    let _ = pid;
}

/// 杀掉进程组：桥 spawn 时已 setsid 自成组长，杀组可连带回收 camoufox 等孙进程，
/// 否则桥被 SIGKILL 后浏览器会变成孤儿（Node 靠 process.on('exit') 兜底，SIGKILL 下不执行）。
pub fn kill_tree(pid: u32) {
    if pid == 0 {
        return;
    }
    #[cfg(unix)]
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
    #[cfg(not(unix))]
    let _ = pid;
}

/// 二进制入口的看门狗循环：终止路径直接以约定退出码结束进程。
pub async fn watchdog(opts: StartOptions) -> ! {
    let mut helpers = Helpers::default();
    let (helper_tx, mut helper_rx) = mpsc::unbounded_channel::<HelperEvent>();
    let (stop_tx, mut stop_rx) = mpsc::unbounded_channel::<()>();

    let mut restarts = 0u32;
    let mut login = opts.login.clone();
    loop {
        let mut round = opts.clone();
        round.login = login.clone();
        match run_server(
            &round,
            &mut helpers,
            &helper_tx,
            &mut helper_rx,
            &mut stop_rx,
            stop_tx.clone(),
        )
        .await
        {
            Ok(()) => {
                helpers.terminate().await;
                release_lock(&opts.data_dir);
                std::process::exit(0);
            }
            Err(RunError::Fatal(msg)) => {
                logfmt::error("看门狗", &format!("{msg}，不会自动重启"));
                helpers.terminate().await;
                release_lock(&opts.data_dir);
                std::process::exit(FATAL_EXIT);
            }
            Err(RunError::Retryable(msg)) => {
                restarts += 1;
                if restarts > MAX_AUTO_RESTARTS {
                    logfmt::error(
                        "看门狗",
                        &format!("连续异常 {restarts} 次，停止自动重启。最后错误: {msg}"),
                    );
                    helpers.terminate().await;
                    release_lock(&opts.data_dir);
                    std::process::exit(1);
                }
                logfmt::warn(
                    "看门狗",
                    &format!("{msg}，将自动重启 ({restarts}/{MAX_AUTO_RESTARTS})"),
                );
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(1000)) => {},
                    _ = shutdown_signal() => {
                        helpers.terminate().await;
                        release_lock(&opts.data_dir);
                        std::process::exit(0);
                    }
                }
            }
            Err(RunError::RestartWith(new_args)) => {
                login = new_args.into_iter().find_map(|a| {
                    a.strip_prefix("-login=")
                        .map(|s| s.to_string())
                        .or_else(|| (a == "-login").then(String::new))
                });
                restarts = 0;
                logfmt::info("看门狗", "正在重启子服务...");
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(500)) => {},
                    _ = shutdown_signal() => {
                        helpers.terminate().await;
                        release_lock(&opts.data_dir);
                        std::process::exit(0);
                    }
                }
            }
        }
    }
}

/// supervise 句柄：测试/嵌入场景用它请求终止并等待服务轮结束。
pub struct SuperviseHandle {
    stop_tx: mpsc::UnboundedSender<()>,
    done: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}

impl SuperviseHandle {
    /// 请求停机并等待清理完成（等价 /admin/stop 路径）。
    pub async fn shutdown(self) {
        let _ = self.stop_tx.send(());
        let rx = self.done.lock().unwrap().take();
        if let Some(rx) = rx {
            let _ = rx.await;
        }
    }
}

/// 进程内启动服务（集成测试用）：同样的重启循环与停机语义，但不调用 process::exit。
pub async fn supervise(opts: StartOptions) -> SuperviseHandle {
    let (stop_tx, stop_rx) = mpsc::unbounded_channel::<()>();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
    let handle = SuperviseHandle {
        stop_tx: stop_tx.clone(),
        done: Mutex::new(Some(done_rx)),
    };
    tokio::spawn(async move {
        let mut helpers = Helpers::default();
        let (helper_tx, mut helper_rx) = mpsc::unbounded_channel::<HelperEvent>();
        let mut stop_rx = stop_rx;
        let mut restarts = 0u32;
        let mut login = opts.login.clone();
        loop {
            let mut round = opts.clone();
            round.login = login.clone();
            match run_server(
                &round,
                &mut helpers,
                &helper_tx,
                &mut helper_rx,
                &mut stop_rx,
                stop_tx.clone(),
            )
            .await
            {
                Ok(()) => break,
                Err(RunError::Fatal(msg)) => {
                    logfmt::error("看门狗", &format!("{msg}，不会自动重启"));
                    break;
                }
                Err(RunError::Retryable(msg)) => {
                    restarts += 1;
                    if restarts > MAX_AUTO_RESTARTS {
                        logfmt::error(
                            "看门狗",
                            &format!("连续异常 {restarts} 次，停止自动重启。最后错误: {msg}"),
                        );
                        break;
                    }
                    logfmt::warn(
                        "看门狗",
                        &format!("{msg}，将自动重启 ({restarts}/{MAX_AUTO_RESTARTS})"),
                    );
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_millis(300)) => {},
                        _ = stop_rx.recv() => break,
                    }
                }
                Err(RunError::RestartWith(new_args)) => {
                    login = new_args.into_iter().find_map(|a| {
                        a.strip_prefix("-login=")
                            .map(|s| s.to_string())
                            .or_else(|| (a == "-login").then(String::new))
                    });
                    restarts = 0;
                    logfmt::info("看门狗", "正在重启子服务...");
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_millis(300)) => {},
                        _ = stop_rx.recv() => break,
                    }
                }
            }
        }
        helpers.terminate().await;
        release_lock(&opts.data_dir);
        let _ = done_tx.send(());
    });
    handle
}

#[allow(clippy::too_many_arguments)]
pub async fn run_server(
    opts: &StartOptions,
    helpers: &mut Helpers,
    helper_tx: &mpsc::UnboundedSender<HelperEvent>,
    helper_rx: &mut mpsc::UnboundedReceiver<HelperEvent>,
    stop_rx: &mut mpsc::UnboundedReceiver<()>,
    stop_tx: mpsc::UnboundedSender<()>,
) -> Result<(), RunError> {
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);
    let root = &opts.src_root;
    let data_dir = &opts.data_dir;

    // Xvfb/VNC 按需拉起；Xvfb 退出轮会清环境变量，回到这里重新启动
    tokio::select! {
        biased;
        _ = &mut shutdown => return Ok(()),
        result = helpers.ensure(opts, helper_tx) => result.map_err(RunError::Fatal)?,
    }
    let vnc_info = helpers.vnc_info.clone();

    let config = config::load_config_in(root, data_dir)
        .map_err(|ConfigError(e)| RunError::Fatal(format!("配置加载失败: {e}")))?;
    let port = config::port_of(&config);

    // 端口预检：被占用立即致命退出（server.js 同款）
    if TcpListener::bind(("0.0.0.0", port)).await.is_err() {
        return Err(RunError::Fatal(format!(
            "端口 {port} 不可用。通常已有实例在运行。请先停止旧实例或修改 server.port"
        )));
    }

    if let Err(e) = crate::history::init(data_dir) {
        logfmt::warn(
            "服务器",
            &format!("历史记录数据库初始化失败，功能可能不可用: {e}"),
        );
    }
    crate::stats::init(&data_dir.join("logs"));

    // 让 server.rs 的 camoufox 版本探测能找到 src-root/camoufox
    if std::env::var("WEBAI2API_SRC_ROOT").is_err() {
        std::env::set_var("WEBAI2API_SRC_ROOT", root);
    }

    // 启动引擎桥
    let temp_dir = data_dir.join("temp");
    std::fs::create_dir_all(&temp_dir).ok();
    // Unix socket path lengths are limited (about 104 bytes on macOS). Keep the
    // IPC name short; browser output and bridge scratch files still use data_dir.
    let sock = std::env::temp_dir().join(format!("w2a-{}.sock", std::process::id()));
    let node = std::env::var("WEBAI2API_NODE").unwrap_or_else(|_| "node".to_string());
    let login = opts
        .login
        .as_deref()
        .map(|s| if s.is_empty() { "1" } else { s });
    let start = async {
        if let Some(bridge_script) = opts.bridge_script.as_ref() {
            // Explicit bridge injection is retained for mock IPC integration tests.
            let (mut child, bridge_client) = bridge::spawn_bridge(
                &node,
                bridge_script,
                root,
                &sock,
                login,
                &temp_dir,
                &opts.extra_envs,
            )
            .await
            .map_err(|error| RunError::Retryable(error.to_string()))?;
            if let Err(error) = bridge_client.preflight().await {
                kill_tree(child.id().unwrap_or_default());
                let _ = child.wait().await;
                return Err(RunError::Fatal(format!("启动预检失败: {error}")));
            }
            let mut safe_mode = None;
            let snapshot = match bridge_client.init(&config).await {
                Ok(value) => value.get("workers").cloned().unwrap_or(json!([])),
                Err(error) => {
                    safe_mode = Some(error.to_string());
                    json!([])
                }
            };
            Ok((
                child,
                BackendRuntime::Legacy(bridge_client),
                snapshot,
                false,
                safe_mode,
            ))
        } else {
            let bridge_script = install_dir().join("bridge/browser-runtime.mjs");
            let (mut child, rpc) = runtime::spawn_browser_rpc(
                &node,
                &bridge_script,
                root,
                &sock,
                &temp_dir,
                opts.login.is_some(),
                &opts.extra_envs,
            )
            .await
            .map_err(|error| RunError::Retryable(error.to_string()))?;
            if let Err(error) = rpc.preflight_with_config(&config).await {
                kill_tree(child.id().unwrap_or_default());
                let _ = child.wait().await;
                return Err(RunError::Fatal(format!("启动预检失败: {error}")));
            }
            match RustRuntime::initialize(rpc.clone(), config.clone()).await {
                Ok(runtime) => {
                    let snapshot = runtime.worker_snapshot();
                    Ok((
                        child,
                        BackendRuntime::Rust(Arc::new(runtime)),
                        snapshot,
                        false,
                        None,
                    ))
                }
                Err(error) => {
                    logfmt::error("服务器", &format!("工作池初始化失败: {error}"));
                    let runtime = RustRuntime::empty(rpc, config.clone());
                    Ok((
                        child,
                        BackendRuntime::Rust(Arc::new(runtime)),
                        json!([]),
                        false,
                        Some(error),
                    ))
                }
            }
        }
    };
    let started = tokio::select! {
        biased;
        _ = &mut shutdown => return Ok(()),
        result = start => result,
    }?;
    let (mut child, backend, worker_snapshot, browser_stopped, safe_mode) = started;
    let bridge_pid = child.id().unwrap_or_default();
    tokio::select! {
        biased;
        _ = &mut shutdown => {
            shutdown_bridge(&backend, bridge_pid, &mut child).await;
            return Ok(());
        }
        _ = std::future::ready(()) => {}
    }

    let (restart_tx, mut restart_rx) = mpsc::unbounded_channel::<Vec<String>>();
    let queue = crate::queue::Queue::new(
        backend.clone(),
        config::max_concurrent(&config),
        config::queue_buffer(&config),
        config::keepalive_mode(&config),
        config::image_markdown(&config),
    );
    let state = Arc::new(AppState {
        config: config.clone(),
        bridge: backend.clone(),
        queue,
        webui_dir: webui_dir(),
        data_dir: data_dir.to_path_buf(),
        temp_dir,
        started_at: std::time::Instant::now(),
        safe_mode: Mutex::new(safe_mode),
        login_mode: opts.login.is_some(),
        login_worker: opts.login.clone().filter(|s| !s.is_empty()),
        restart: Mutex::new(Some(Box::new(move |a| {
            let _ = restart_tx.send(a);
        }))),
        vnc: Mutex::new(vnc_info),
        workers: Mutex::new(json!({ "workers": worker_snapshot })),
        browser_stopped: Mutex::new(browser_stopped),
        config_path: config::resolve_config_path_in(root, data_dir)
            .unwrap_or_else(|_| data_dir.join("config.yaml")),
        src_root: root.to_path_buf(),
        stop: stop_tx,
        config_save: Mutex::new(()),
        models_cache: Mutex::new(None),
    });
    std::fs::create_dir_all(&state.temp_dir).ok();

    let app = server::router(state.clone());
    let listener = TcpListener::bind(("0.0.0.0", port))
        .await
        .map_err(|e| RunError::Fatal(format!("端口 {port} 绑定失败（{e}），退出以避免无限重启")))?;
    logfmt::info(
        "服务器",
        &format!(
            "HTTP 服务器已启动，端口: {port}{}",
            if opts.login.is_some() {
                " (登录模式)"
            } else {
                ""
            }
        ),
    );
    logfmt::info(
        "服务器",
        &format!(
            "流式心跳模式: {}，最大并发: {}，队列缓冲: {}",
            config::keepalive_mode(&config),
            config::max_concurrent(&config),
            config::queue_buffer(&config)
        ),
    );

    // IPC 兼容：保留原 SUPERVISOR_IPC 文本协议（RESTART/STOP/GET_VNC_INFO）。
    // 服务轮结束时随轮关闭，避免重启轮泄漏监听任务。
    let ipc_state = Arc::clone(&state);
    let (ipc_shutdown_tx, ipc_shutdown_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        let _ = serve_ipc(ipc_state, ipc_shutdown_rx).await;
    });

    let server = axum::serve(listener, app).into_future();
    tokio::pin!(server);

    let result = loop {
        tokio::select! {
            result = &mut server => {
                shutdown_bridge(&backend, bridge_pid, &mut child).await;
                break result.map_err(|e| RunError::Retryable(format!("HTTP 服务异常: {e}")));
            }
            Some(new_args) = restart_rx.recv() => {
                shutdown_bridge(&backend, bridge_pid, &mut child).await;
                break Err(RunError::RestartWith(new_args));
            }
            status = child.wait() => {
                let code = status.map(|s| s.code().unwrap_or(1)).unwrap_or(1);
                // The bridge can exit while browser grandchildren remain alive.
                kill_tree(bridge_pid);
                break Err(RunError::Retryable(format!("引擎桥退出 (code: {code})")));
            }
            _ = stop_rx.recv() => {
                logfmt::info("看门狗", "收到停止指令，开始清理");
                shutdown_bridge(&backend, bridge_pid, &mut child).await;
                break Ok(());
            }
            _ = &mut shutdown => {
                logfmt::info("看门狗", "收到退出信号，开始清理");
                shutdown_bridge(&backend, bridge_pid, &mut child).await;
                break Ok(());
            }
            Some(event) = helper_rx.recv() => match event {
                HelperEvent::XvfbDead => {
                    // 对齐 xvfb-run 包裹语义：Xvfb 退出则整轮服务重启，重新挑选显示号
                    logfmt::error("看门狗", "Xvfb 已退出，浏览器失去显示，将重启服务");
                    helpers.xvfb_pid = None;
                    std::env::remove_var("XVFB_RUNNING");
                    std::env::remove_var("DISPLAY");
                    helpers.terminate_vnc().await;
                    helpers.vnc_pid = None;
                    helpers.vnc_info.enabled = false;
                    shutdown_bridge(&backend, bridge_pid, &mut child).await;
                    break Err(RunError::Retryable("Xvfb 已退出".into()));
                }
                HelperEvent::VncDead => {
                    // 对齐 supervisor.js：x11vnc 退出只把 VNC 标记为不可用，不影响服务
                    logfmt::warn("看门狗", "x11vnc 已退出，VNC 不可用");
                    helpers.vnc_pid = None;
                    helpers.vnc_info.enabled = false;
                    *state.vnc.lock().unwrap() = helpers.vnc_info.clone();
                    continue;
                }
            }
        }
    };
    let _ = ipc_shutdown_tx.send(true);
    result
}

/// 停止引擎桥：先请求 Node 侧清理浏览器（launcher cleanup），再杀整个进程组回收残留。
async fn shutdown_bridge(
    backend: &BackendRuntime,
    bridge_pid: u32,
    child: &mut tokio::process::Child,
) {
    let _ = tokio::time::timeout(Duration::from_secs(5), backend.shutdown()).await;
    kill_tree(bridge_pid);
    let _ = child.wait().await;
}

/// 停机信号：Ctrl-C (SIGINT) 与 SIGTERM 都触发统一清理（supervisor.js 同款处理两个信号）。
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = term.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// 安装目录：包含 bridge/ 与 scripts/ 的目录。
/// 优先 WEBAI2API_HOME；否则从可执行文件位置逐级向上找浏览器桥，
/// 这样开发布局（target/debug 向上两级）自然命中。
fn install_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("WEBAI2API_HOME") {
        return PathBuf::from(dir);
    }
    let mut dir = std::env::current_exe().unwrap_or_default();
    for _ in 0..6 {
        if dir.join("bridge/browser-runtime.mjs").is_file()
            || dir.join("bridge/bridge.mjs").is_file()
        {
            return dir;
        }
        if !dir.pop() {
            break;
        }
    }
    PathBuf::from(".")
}

fn webui_dir() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        std::env::var_os("WEBAI2API_WEBUI_DIR").map(PathBuf::from),
        Some(install_dir().join("webui/dist")),
        Some(install_dir().join("webui-dist")),
        Some(manifest.join("../webui/dist")),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|path| path.join("index.html").is_file())
        .unwrap_or_else(|| manifest.join("../webui/dist"))
}

// ==================== Xvfb / VNC ====================

/// 找到空闲显示号并调用 scripts/start-xvfb.sh，返回 (显示号, 子进程)。
async fn launch_xvfb() -> Result<(String, tokio::process::Child), String> {
    let display_num = (50..100)
        .find(|n| {
            !Path::new(&format!("/tmp/.X{n}-lock")).exists()
                && !Path::new(&format!("/tmp/.X11-unix/X{n}")).exists()
        })
        .ok_or("找不到空闲的 X 显示号")?;
    let display = format!(":{display_num}");
    let child = run_helper("start-xvfb.sh", &[&display]).await?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    Ok((display, child))
}

/// 找到空闲端口并调用 scripts/start-vnc.sh，返回 (端口, 子进程)。
async fn launch_vnc() -> Result<(u16, tokio::process::Child), String> {
    let display = std::env::var("DISPLAY").map_err(|_| "DISPLAY 未设置")?;
    let mut picked = None;
    for p in 5900..6000 {
        if tokio::net::TcpListener::bind(("127.0.0.1", p))
            .await
            .is_ok()
        {
            picked = Some(p);
            break;
        }
    }
    let port = picked.ok_or("无法找到可用的 VNC 端口 (5900-5999)")?;
    let port_arg = port.to_string();
    let child = run_helper("start-vnc.sh", &[&display, &port_arg]).await?;
    Ok((port, child))
}

/// 运行 scripts/ 下的辅助脚本。脚本内部自行校验参数格式。
async fn run_helper(script: &str, script_args: &[&str]) -> Result<tokio::process::Child, String> {
    let path = install_dir().join("scripts").join(script);
    tokio::process::Command::new("sh")
        .arg(&path)
        .args(script_args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(|e| format!("启动 {script} 失败: {e}"))
}

// ==================== IPC 文本协议兼容 ====================

async fn serve_ipc(
    state: Arc<AppState>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> std::io::Result<()> {
    let sock_path = std::env::var("SUPERVISOR_IPC")
        .map(PathBuf::from)
        .unwrap_or_else(|_| state.data_dir.join("temp/webai2api-supervisor.sock"));
    if sock_path.exists() {
        std::fs::remove_file(&sock_path).ok();
    }
    let listener = tokio::net::UnixListener::bind(&sock_path)?;
    logfmt::info(
        "看门狗",
        &format!("IPC 服务器已启动: {}", sock_path.display()),
    );
    loop {
        tokio::select! {
            r = listener.accept() => {
                let (mut stream, _) = match r {
                    Ok(x) => x,
                    Err(_) => break,
                };
                let state = Arc::clone(&state);
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 1024];
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    let cmd = String::from_utf8_lossy(&buf[..n]).trim().to_string();
                    let reply = if cmd == "RESTART" || cmd.starts_with("RESTART:") {
                        let extra: Vec<String> = cmd.split_once(':')
                            .map(|(_, a)| a.split_whitespace().map(str::to_string).collect())
                            .unwrap_or_default();
                        if let Some(f) = state.restart.lock().unwrap().as_ref() { f(extra); }
                        "OK\n".to_string()
                    } else if cmd == "STOP" {
                        // 走统一停机通道，保证浏览器清理执行
                        let _ = state.stop.send(());
                        "OK\n".to_string()
                    } else if cmd == "GET_VNC_INFO" {
                        let v = state.vnc.lock().unwrap();
                        format!("{}\n", serde_json::json!({
                            "enabled": v.enabled, "port": v.port, "display": v.display, "xvfbMode": v.xvfb_mode
                        }))
                    } else {
                        "UNKNOWN_COMMAND\n".to_string()
                    };
                    let _ = stream.write_all(reply.as_bytes()).await;
                });
            }
            _ = shutdown.changed() => break,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webui_is_resolved_from_checkout_layout() {
        let resolved = webui_dir();
        assert!(
            resolved.join("index.html").is_file(),
            "{}",
            resolved.display()
        );
    }
}
