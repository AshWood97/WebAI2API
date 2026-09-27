//! Production Rust backend runtime.
//!
//! Worker selection, model capabilities, adapter execution, and failover live
//! here. The Node child is only a generic browser RPC endpoint.

use crate::adapters::{sites, GenerateRequest, PageClient, RpcPage, SiteAdapter};
use crate::bridge::{Bridge, GenerateResult};
use crate::browser_rpc::{BrowserRpc, BrowserStarted};
use crate::catalog;
use crate::scheduler::{
    AdapterCapability, AdapterExecutor, FailoverConfig, GenerationResult, ImagePolicy,
    ModelCapability, Scheduler, Strategy, WorkerSpec,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

struct RuntimeWorker {
    spec: WorkerSpec,
    page: Arc<tokio::sync::RwLock<Arc<dyn PageClient>>>,
    page_id: Arc<tokio::sync::RwLock<String>>,
    browser_id: Arc<tokio::sync::RwLock<String>>,
    instance_name: String,
    engine: String,
    user_data_dir: String,
    browser_runtime: Option<Value>,
    execution: Arc<tokio::sync::Mutex<()>>,
    adapters: HashMap<String, Value>,
    target_url: String,
    monitor_url: Option<String>,
    page_ready: Arc<AtomicBool>,
    auth_ready: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    launch_config: Value,
    launch_options: Value,
}

#[derive(Clone)]
struct WorkerMonitor {
    page: Arc<tokio::sync::RwLock<Arc<dyn PageClient>>>,
    page_id: Arc<tokio::sync::RwLock<String>>,
    browser_id: Arc<tokio::sync::RwLock<String>>,
    execution: Arc<tokio::sync::Mutex<()>>,
    page_ready: Arc<AtomicBool>,
    auth_ready: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    target_url: String,
    launch_config: Value,
    launch_options: Value,
}

/// Initialized browser handles and Rust-owned scheduler/adapter state.
pub struct RustRuntime {
    rpc: BrowserRpc,
    config: Value,
    workers: Vec<RuntimeWorker>,
    scheduler: Scheduler,
    browser_ids: Arc<tokio::sync::RwLock<Vec<String>>>,
    shutdown: Arc<AtomicBool>,
    temp_dir: PathBuf,
}

/// Common generation/model facade selected by startup. A user supplied bridge
/// script remains available for the existing IPC integration fixtures.
#[derive(Clone)]
pub enum BackendRuntime {
    Legacy(Bridge),
    Rust(Arc<RustRuntime>),
}

impl From<Bridge> for BackendRuntime {
    fn from(value: Bridge) -> Self {
        Self::Legacy(value)
    }
}

impl BackendRuntime {
    pub fn worker_snapshot(&self) -> Option<Value> {
        match self {
            Self::Legacy(_) => None,
            Self::Rust(runtime) => Some(runtime.worker_snapshot()),
        }
    }

    pub async fn generate(
        &self,
        id: &str,
        prompt: &str,
        image_paths: &[String],
        model_id: &str,
        reasoning: bool,
    ) -> Result<GenerateResult, crate::errors::BridgeError> {
        match self {
            Self::Legacy(bridge) => {
                bridge
                    .generate(id, prompt, image_paths, model_id, reasoning)
                    .await
            }
            Self::Rust(runtime) => Ok(runtime
                .generate(id, prompt, image_paths, model_id, reasoning)
                .await),
        }
    }

    pub async fn shutdown(&self) {
        match self {
            Self::Legacy(bridge) => bridge.shutdown().await,
            Self::Rust(runtime) => runtime.shutdown().await,
        }
    }

    pub async fn get_models(&self) -> Result<Value, crate::errors::BridgeError> {
        match self {
            Self::Legacy(bridge) => bridge.get_models().await,
            Self::Rust(runtime) => Ok(runtime.models()),
        }
    }

    pub async fn model_info(&self, model: &str) -> Result<Value, crate::errors::BridgeError> {
        match self {
            Self::Legacy(bridge) => bridge.model_info(model).await,
            Self::Rust(runtime) => Ok(runtime.model_info(model)),
        }
    }

    pub async fn navigate_to_monitor(&self) -> Result<(), crate::errors::BridgeError> {
        match self {
            Self::Legacy(bridge) => bridge.navigate_to_monitor().await,
            Self::Rust(runtime) => runtime.navigate_to_monitor().await,
        }
    }

    pub async fn list_adapters(&self) -> Result<Value, crate::errors::BridgeError> {
        match self {
            Self::Legacy(bridge) => bridge.list_adapters().await,
            Self::Rust(runtime) => Ok(runtime.adapters()),
        }
    }

    pub async fn restart_browser(&self) -> Result<Value, crate::errors::BridgeError> {
        match self {
            Self::Legacy(bridge) => bridge.restart_browser().await,
            Self::Rust(runtime) => runtime.restart_browser().await,
        }
    }

    pub async fn get_cookies(
        &self,
        instance: Option<&str>,
        domain: Option<&str>,
    ) -> Result<Value, crate::errors::BridgeError> {
        match self {
            Self::Legacy(bridge) => bridge.get_cookies(instance, domain).await,
            Self::Rust(runtime) => runtime.cookies(instance, domain).await,
        }
    }

    pub async fn download_via_context(
        &self,
        url: &str,
        retries: u64,
    ) -> Result<Value, crate::errors::BridgeError> {
        match self {
            Self::Legacy(bridge) => bridge.download_via_context(url, retries).await,
            Self::Rust(runtime) => runtime.download_via_context(url, retries).await,
        }
    }
}

/// Launch the generic browser-only JSONL bridge and wait for its ready event.
pub async fn spawn_browser_rpc(
    node_bin: &str,
    script: &std::path::Path,
    src_root: &std::path::Path,
    socket: &std::path::Path,
    temp_dir: &std::path::Path,
    login: Option<&str>,
    extra_envs: &[(String, String)],
) -> Result<(tokio::process::Child, BrowserRpc), crate::errors::BridgeError> {
    if socket.exists() {
        std::fs::remove_file(socket).ok();
    }
    let data_dir = temp_dir.parent().unwrap_or(src_root);
    std::fs::create_dir_all(data_dir)
        .map_err(|source| crate::errors::BridgeError::Spawn { source })?;
    std::fs::create_dir_all(temp_dir)
        .map_err(|source| crate::errors::BridgeError::Spawn { source })?;
    let cwd = temp_dir.join("bridge-cwd");
    std::fs::create_dir_all(&cwd).map_err(|source| crate::errors::BridgeError::Spawn { source })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let link = cwd.join("data");
        if !std::fs::read_link(&link).is_ok_and(|target| target == data_dir) {
            if std::fs::symlink_metadata(&link).is_ok_and(|meta| meta.file_type().is_symlink()) {
                std::fs::remove_file(&link)
                    .map_err(|source| crate::errors::BridgeError::Spawn { source })?;
            }
            symlink(data_dir, link)
                .map_err(|source| crate::errors::BridgeError::Spawn { source })?;
        }
    }
    let mut command = tokio::process::Command::new(node_bin);
    command
        .arg(script)
        .env("WEBAI2API_SRC_ROOT", src_root)
        .env("WEBAI2API_SOCK", socket)
        .env("WEBAI2API_LOGIN", login.unwrap_or(""))
        .env("WEBAI2API_TEMP_DIR", temp_dir)
        .env("CAMOUFOX_INSTALL_DIR", src_root.join("camoufox"))
        .current_dir(&cwd)
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true);
    if let Some(login_worker) = login {
        command.arg(if login_worker.is_empty() {
            "-login".to_owned()
        } else {
            format!("-login={login_worker}")
        });
    }
    for (name, value) in extra_envs {
        command.env(name, value);
    }
    // Generic bridge owns a process group, so killing it also reaps browsers.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .map_err(|source| crate::errors::BridgeError::Spawn { source })?;
    let bridge_pid = child.id().unwrap_or_default();
    let mut process_group_guard = BrowserProcessGroupGuard(bridge_pid);
    for _ in 0..100 {
        if socket.exists() {
            break;
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|source| crate::errors::BridgeError::Spawn { source })?
        {
            return Err(crate::errors::BridgeError::Remote(format!(
                "browser bridge exited during startup: {status}"
            )));
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    if !socket.exists() {
        crate::run::kill_tree(bridge_pid);
        let _ = child.wait().await;
        return Err(crate::errors::BridgeError::SocketNotCreated);
    }
    let rpc = match BrowserRpc::connect(socket.to_path_buf()).await {
        Ok(rpc) => rpc,
        Err(error) => {
            crate::run::kill_tree(bridge_pid);
            let _ = child.wait().await;
            return Err(error);
        }
    };
    match tokio::time::timeout(std::time::Duration::from_secs(30), rpc.next_event()).await {
        Ok(Some(event)) if event.get("event").and_then(Value::as_str) == Some("ready") => {}
        _ => {
            crate::run::kill_tree(bridge_pid);
            let _ = child.wait().await;
            return Err(crate::errors::BridgeError::ReadyTimeout);
        }
    }
    process_group_guard.0 = 0;
    Ok((child, rpc))
}

struct BrowserProcessGroupGuard(u32);

impl Drop for BrowserProcessGroupGuard {
    fn drop(&mut self) {
        if self.0 != 0 {
            crate::run::kill_tree(self.0);
        }
    }
}

impl RustRuntime {
    pub fn empty(rpc: BrowserRpc, config: Value, temp_dir: PathBuf) -> Self {
        Self {
            rpc,
            config,
            workers: Vec::new(),
            scheduler: Scheduler::new(Strategy::LeastBusy, FailoverConfig::default()),
            browser_ids: Arc::new(tokio::sync::RwLock::new(Vec::new())),
            shutdown: Arc::new(AtomicBool::new(false)),
            temp_dir,
        }
    }
    /// Start configured browser instances and materialize the worker capability
    /// snapshot from the embedded Rust catalog.
    pub async fn initialize(
        rpc: BrowserRpc,
        config: Value,
        temp_dir: PathBuf,
    ) -> Result<Self, String> {
        Self::initialize_login(rpc, config, temp_dir, None).await
    }

    pub async fn initialize_login(
        rpc: BrowserRpc,
        config: Value,
        temp_dir: PathBuf,
        login: Option<&str>,
    ) -> Result<Self, String> {
        let workers_config = config
            .pointer("/backend/pool/workers")
            .and_then(Value::as_array)
            .ok_or_else(|| "backend.pool.workers is missing".to_string())?;
        let mut browsers: HashMap<String, BrowserStarted> = HashMap::new();
        let mut assigned_pages = std::collections::HashSet::new();
        let mut browser_ids = Vec::new();
        let mut workers = Vec::new();
        let login_name = select_login_worker(workers_config, login)?;

        for worker_cfg in workers_config {
            let Ok(worker_name) = str_field(worker_cfg, "name") else {
                continue;
            };
            if login_name
                .as_deref()
                .is_some_and(|name| name != worker_name)
            {
                continue;
            }
            let Ok(worker_type) = str_field(worker_cfg, "type") else {
                continue;
            };
            let merge_types = string_array(worker_cfg.get("mergeTypes"));
            let adapter_types = if worker_type == "merge" {
                merge_types.clone()
            } else {
                vec![worker_type.to_owned()]
            };
            if adapter_types.is_empty() || adapter_types.iter().any(|id| adapter_for(id).is_none())
            {
                crate::logfmt::warn(
                    "工作池",
                    &format!("[{worker_name}] 配置了未注册的 Rust 适配器，跳过 Worker"),
                );
                continue;
            }
            let engine = worker_cfg
                .get("engine")
                .and_then(Value::as_str)
                .unwrap_or("camoufox");
            let user_data_dir = worker_cfg
                .get("userDataDir")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let browser_key = format!("{engine}\0{user_data_dir}");
            let launch_options = json!({
                "engine": engine,
                "userDataDir": user_data_dir,
                "instanceName": worker_cfg.get("instanceName").and_then(Value::as_str),
                "proxyConfig": worker_cfg.get("resolvedProxy").cloned().unwrap_or(Value::Null),
            });
            let browser = if let Some(browser) = browsers.get(&browser_key) {
                browser.clone()
            } else {
                let started = match rpc
                    .browser_start(config.clone(), launch_options.clone())
                    .await
                {
                    Ok(started) => started,
                    Err(error) => {
                        crate::logfmt::warn(
                            "工作池",
                            &format!("[{worker_name}] 浏览器启动失败，跳过 Worker: {error}"),
                        );
                        continue;
                    }
                };
                browser_ids.push(started.browser_id.clone());
                browsers.insert(browser_key, started.clone());
                started
            };
            let first_page = browser
                .page_ids
                .first()
                .filter(|id| assigned_pages.insert((*id).clone()));
            let page_id = if let Some(page_id) = first_page {
                page_id.clone()
            } else {
                match rpc.page_create(&browser.browser_id, None, None).await {
                    Ok(created) => created.page_id,
                    Err(error) => {
                        crate::logfmt::warn(
                            "工作池",
                            &format!("[{worker_name}] 页面创建失败，跳过 Worker: {error}"),
                        );
                        continue;
                    }
                }
            };
            let page: Arc<dyn PageClient> = Arc::new(RpcPage::new(
                rpc.clone(),
                browser.browser_id.clone(),
                page_id.clone(),
            ));
            let mut capabilities = Vec::new();
            let mut adapter_configs = HashMap::new();
            for adapter_type in adapter_types {
                let models = catalog::models_for_adapter(&adapter_type, &config);
                let model_capabilities = models["data"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|model| {
                        Some(ModelCapability::new(
                            model.get("id")?.as_str()?,
                            model.get("type").and_then(Value::as_str).unwrap_or("image"),
                            match model
                                .get("image_policy")
                                .and_then(Value::as_str)
                                .unwrap_or("optional")
                            {
                                "required" => ImagePolicy::Required,
                                "forbidden" => ImagePolicy::Forbidden,
                                _ => ImagePolicy::Optional,
                            },
                        ))
                    })
                    .collect::<Vec<_>>();
                capabilities.push(AdapterCapability::new(
                    adapter_type.clone(),
                    model_capabilities,
                ));
                adapter_configs.insert(
                    adapter_type.clone(),
                    adapter_request_config(&config, &adapter_type),
                );
            }
            let first_adapter = if worker_type == "merge" {
                merge_types.first().map(String::as_str).unwrap_or("")
            } else {
                worker_type
            };
            let target_url = adapter_target_url(first_adapter, &config);
            let monitor_url = worker_cfg
                .get("mergeMonitor")
                .and_then(Value::as_str)
                .map(|adapter| adapter_target_url(adapter, &config))
                .filter(|url| url != "about:blank");
            // Keep a useful page open for login mode and mirror Worker.init's
            // initial navigation. Navigation failures do not disable a worker.
            let _ = page
                .call(vec![json!({"op":"goto", "url":target_url, "options":{"waitUntil":"domcontentloaded", "timeout":60000}})])
                .await;
            let launch_config = config.clone();
            workers.push(RuntimeWorker {
                spec: WorkerSpec::new(worker_name, worker_type, merge_types, capabilities),
                page: Arc::new(tokio::sync::RwLock::new(page)),
                page_id: Arc::new(tokio::sync::RwLock::new(page_id)),
                browser_id: Arc::new(tokio::sync::RwLock::new(browser.browser_id.clone())),
                instance_name: worker_cfg
                    .get("instanceName")
                    .and_then(Value::as_str)
                    .unwrap_or(worker_name)
                    .to_owned(),
                engine: engine.to_owned(),
                user_data_dir: user_data_dir.to_owned(),
                browser_runtime: browser.runtime.clone(),
                execution: Arc::new(tokio::sync::Mutex::new(())),
                adapters: adapter_configs,
                target_url: target_url.to_owned(),
                monitor_url,
                page_ready: Arc::new(AtomicBool::new(true)),
                auth_ready: Arc::new(AtomicBool::new(true)),
                stopped: Arc::new(AtomicBool::new(false)),
                launch_config,
                launch_options,
            });
        }
        if workers.is_empty() {
            return Err("no configured workers matched login mode".to_owned());
        }

        let strategy = Strategy::from_name(
            config
                .pointer("/backend/pool/strategy")
                .and_then(Value::as_str)
                .unwrap_or("least_busy"),
        );
        let failover = config.pointer("/backend/pool/failover");
        let scheduler = Scheduler::new(
            strategy,
            FailoverConfig {
                enabled: failover
                    .and_then(|v| v.get("enabled"))
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
                max_retries: failover
                    .and_then(|v| v.get("maxRetries"))
                    .and_then(Value::as_u64)
                    .unwrap_or(2) as usize,
            },
        );
        let monitors = workers
            .iter()
            .map(|worker| WorkerMonitor {
                page: worker.page.clone(),
                page_id: worker.page_id.clone(),
                browser_id: worker.browser_id.clone(),
                execution: worker.execution.clone(),
                page_ready: worker.page_ready.clone(),
                auth_ready: worker.auth_ready.clone(),
                stopped: worker.stopped.clone(),
                target_url: worker.target_url.clone(),
                launch_config: worker.launch_config.clone(),
                launch_options: worker.launch_options.clone(),
            })
            .collect::<Vec<_>>();
        let browser_ids = Arc::new(tokio::sync::RwLock::new(browser_ids));
        let shutdown = Arc::new(AtomicBool::new(false));
        let headless = config
            .pointer("/browser/headless")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            && login.is_none()
            && std::env::var("XVFB_RUNNING").is_err();
        spawn_lifecycle_monitor(
            rpc.clone(),
            monitors,
            browser_ids.clone(),
            shutdown.clone(),
            headless,
        );
        Ok(Self {
            rpc,
            config,
            workers,
            scheduler,
            browser_ids,
            shutdown,
            temp_dir,
        })
    }

    pub fn worker_snapshot(&self) -> Value {
        json!({"workers": self.workers.iter().map(|worker| json!({
            "name": worker.spec.name,
            "instance": worker.instance_name,
            "type": worker.spec.worker_type,
            "adapter": worker.spec.worker_type,
            "engine": worker.engine,
            "userDataDir": std::path::Path::new(&worker.user_data_dir).file_name().and_then(|v| v.to_str()).unwrap_or(&worker.user_data_dir),
            "busy": worker.spec.busy_count() > 0,
            "busyCount": worker.spec.busy_count(),
            "pageReady": worker.page_ready.load(Ordering::Relaxed),
            "authReady": worker.auth_ready.load(Ordering::Relaxed),
            "mergeTypes": worker.spec.merge_types,
            "stopped": worker.stopped.load(Ordering::Relaxed),
            "runtime": worker.browser_runtime.as_ref().map(sanitize_runtime),
        })).collect::<Vec<_>>()})
    }

    pub fn models(&self) -> Value {
        let specs = self
            .workers
            .iter()
            .map(|worker| worker.spec.clone())
            .collect::<Vec<_>>();
        let listed = self.scheduler.models(&specs);
        let mut data = Vec::with_capacity(listed.len());
        for model in listed {
            let (adapter_type, source_id) =
                if let Some((adapter, source)) = model.id.split_once('/') {
                    (adapter, source)
                } else {
                    let Some(adapter) = self
                        .workers
                        .iter()
                        .flat_map(|worker| worker.spec.capabilities.iter())
                        .find(|capability| capability.models.iter().any(|item| item.id == model.id))
                        .map(|capability| capability.adapter_type.as_str())
                    else {
                        continue;
                    };
                    (adapter, model.id.as_str())
                };
            let Some(source) = catalog::models_for_adapter(adapter_type, &self.config)["data"]
                .as_array()
                .and_then(|models| models.iter().find(|entry| entry["id"] == source_id))
                .cloned()
            else {
                continue;
            };
            let mut source = source;
            source["id"] = Value::String(model.id);
            source["owned_by"] = Value::String(model.owned_by);
            source["created"] = Value::from(chrono::Utc::now().timestamp());
            data.push(source);
        }
        json!({"object":"list", "data": data})
    }

    pub fn model_info(&self, model_id: &str) -> Value {
        let specs = self
            .workers
            .iter()
            .map(|worker| worker.spec.clone())
            .collect::<Vec<_>>();
        let models = self.models();
        let model_type = self.scheduler.model_type(&specs, model_id);
        let image_policy = self.scheduler.image_policy(&specs, model_id).as_str();
        json!({"models": models, "modelType": model_type, "imagePolicy": image_policy})
    }

    pub fn adapters(&self) -> Value {
        json!(catalog::list_adapters(&self.config))
    }

    pub async fn generate(
        &self,
        _id: &str,
        prompt: &str,
        image_paths: &[String],
        model_id: &str,
        reasoning: bool,
    ) -> GenerateResult {
        let request = GenerateRequest {
            prompt: prompt.to_owned(),
            model_id: model_id.to_owned(),
            image_paths: image_paths.iter().map(Into::into).collect(),
            reasoning,
            wait_timeout_ms: self
                .config
                .pointer("/backend/pool/waitTimeout")
                .and_then(Value::as_u64)
                .unwrap_or(120_000),
            adapter_config: Value::Null,
        };
        let executor = RuntimeExecutor {
            runtime: self,
            request: &request,
        };
        let specs = self
            .workers
            .iter()
            .filter(|worker| !worker.stopped.load(Ordering::Relaxed))
            .map(|worker| worker.spec.clone())
            .collect::<Vec<_>>();
        let result = self
            .scheduler
            .execute(&specs, model_id, !image_paths.is_empty(), &executor)
            .await;
        if let Some(error) = result.error {
            return GenerateResult {
                text: None,
                image_path: None,
                image_mime: None,
                image_url: None,
                reasoning: None,
                error: Some(error),
                code: result.code,
                retryable: result.retryable.unwrap_or(false),
            };
        }
        let output = result.value.unwrap_or(Value::Null);
        GenerateResult {
            text: output
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_owned),
            image_path: None,
            image_mime: None,
            image_url: output
                .get("image")
                .and_then(Value::as_str)
                .map(str::to_owned),
            reasoning: output
                .get("reasoning")
                .and_then(Value::as_str)
                .map(str::to_owned),
            error: None,
            code: None,
            retryable: false,
        }
    }

    pub async fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
        for id in self.browser_ids.read().await.iter() {
            let _ = self.rpc.browser_close(id).await;
        }
        let _ = self.rpc.shutdown().await;
    }

    async fn restart_browser(&self) -> Result<Value, crate::errors::BridgeError> {
        let old_ids = self.browser_ids.read().await.clone();
        let mut stopped_ids = Vec::new();
        for old_id in &old_ids {
            let mut stopped = false;
            for worker in &self.workers {
                if worker.stopped.load(Ordering::Relaxed)
                    && *worker.browser_id.read().await == *old_id
                {
                    stopped = true;
                    break;
                }
            }
            if stopped {
                stopped_ids.push(old_id.clone());
            }
        }
        if stopped_ids.is_empty() {
            return Ok(json!({
                "browserStopped": false,
                "workers": self.worker_snapshot()["workers"].clone(),
                "result": {"success": false, "message": "浏览器未被手动关闭，无需恢复（如需重建请直接重启服务）"}
            }));
        }
        let mut replacement_ids = old_ids;
        for old_id in stopped_ids {
            let mut indexes = Vec::new();
            for (index, worker) in self.workers.iter().enumerate() {
                if *worker.browser_id.read().await == old_id {
                    indexes.push(index);
                }
            }
            let Some(first_index) = indexes.first().copied() else {
                continue;
            };
            let _guards = lock_workers(&self.workers, &indexes).await;
            if !indexes
                .iter()
                .any(|index| self.workers[*index].stopped.load(Ordering::Relaxed))
            {
                continue;
            }
            let first = &self.workers[first_index];
            let started = self
                .rpc
                .browser_start(first.launch_config.clone(), first.launch_options.clone())
                .await?;
            let new_id = started.browser_id.clone();
            let mut pages = started.page_ids.into_iter();
            let mut replacements = Vec::with_capacity(indexes.len());
            for index in &indexes {
                let worker = &self.workers[*index];
                let page_id = if let Some(page_id) = pages.next() {
                    page_id
                } else {
                    match self.rpc.page_create(&new_id, None, None).await {
                        Ok(created) => created.page_id,
                        Err(error) => {
                            let _ = self.rpc.browser_close(&new_id).await;
                            return Err(error);
                        }
                    }
                };
                let page: Arc<dyn PageClient> = Arc::new(RpcPage::new(
                    self.rpc.clone(),
                    new_id.clone(),
                    page_id.clone(),
                ));
                let _ = page.call(vec![json!({"op":"goto", "url":worker.target_url, "options":{"waitUntil":"domcontentloaded", "timeout":60000}})]).await;
                replacements.push((*index, page_id, page));
            }
            for (index, page_id, page) in replacements {
                let worker = &self.workers[index];
                *worker.page.write().await = page;
                *worker.page_id.write().await = page_id;
                *worker.browser_id.write().await = new_id.clone();
                worker.page_ready.store(true, Ordering::Relaxed);
                worker.auth_ready.store(true, Ordering::Relaxed);
                worker.stopped.store(false, Ordering::Relaxed);
            }
            if let Some(index) = replacement_ids.iter().position(|id| id == &old_id) {
                replacement_ids[index] = new_id;
            }
        }
        *self.browser_ids.write().await = replacement_ids;
        Ok(json!({
            "browserStopped": false,
            "workers": self.worker_snapshot()["workers"].clone(),
            "result": {"success": true, "message": "浏览器已重启"}
        }))
    }

    async fn navigate_to_monitor(&self) -> Result<(), crate::errors::BridgeError> {
        for worker in &self.workers {
            let Some(url) = worker.monitor_url.as_deref() else {
                continue;
            };
            if worker.stopped.load(Ordering::Relaxed) {
                continue;
            }
            let _guard = worker.execution.lock().await;
            let page = worker.page.read().await.clone();
            let current = page
                .call(vec![json!({"op":"url"})])
                .await
                .ok()
                .and_then(|values| values.first().and_then(Value::as_str).map(str::to_owned));
            let should_navigate = current.as_deref().is_none_or(|current| {
                let host = url::Url::parse(url)
                    .ok()
                    .and_then(|u| u.host_str().map(str::to_owned));
                host.is_none_or(|host| !current.contains(&host))
            });
            if should_navigate {
                let _ = page.call(vec![json!({"op":"goto", "url":url, "options":{"waitUntil":"domcontentloaded", "timeout":30000}})]).await;
            }
        }
        Ok(())
    }

    async fn download_via_context(
        &self,
        url: &str,
        retries: u64,
    ) -> Result<Value, crate::errors::BridgeError> {
        static NEXT_DOWNLOAD: AtomicU64 = AtomicU64::new(1);
        let worker = self.workers.first().ok_or_else(|| {
            crate::errors::BridgeError::Remote("browser worker is unavailable".into())
        })?;
        let page_id = worker.page_id.read().await.clone();
        std::fs::create_dir_all(&self.temp_dir)
            .map_err(|source| crate::errors::BridgeError::Spawn { source })?;
        let mut last_error = None;
        for _ in 0..retries.max(1) {
            let output = self.temp_dir.join(format!(
                "retry-media-{}-{}.bin",
                std::process::id(),
                NEXT_DOWNLOAD.fetch_add(1, Ordering::Relaxed)
            ));
            match self
                .rpc
                .download_fetch(
                    &page_id,
                    url,
                    output.to_string_lossy().as_ref(),
                    json!({}),
                    Some(120_000),
                )
                .await
            {
                Ok(result) => {
                    let mime = result
                        .headers
                        .as_object()
                        .and_then(|headers| {
                            headers
                                .iter()
                                .find(|(key, _)| key.eq_ignore_ascii_case("content-type"))
                                .and_then(|(_, value)| value.as_str())
                        })
                        .unwrap_or("application/octet-stream");
                    return Ok(json!({"path": result.path, "mime": mime, "bytes": result.bytes}));
                }
                Err(error) => last_error = Some(error.to_string()),
            }
        }
        Ok(json!({"error": last_error.unwrap_or_else(|| "download failed".into())}))
    }

    async fn cookies(
        &self,
        instance: Option<&str>,
        domain: Option<&str>,
    ) -> Result<Value, crate::errors::BridgeError> {
        let worker = if let Some(instance) = instance {
            self.workers
                .iter()
                .find(|worker| worker.instance_name == instance)
                .ok_or_else(|| {
                    crate::errors::BridgeError::Remote(format!("浏览器实例不存在: {instance}"))
                })?
        } else {
            self.workers.first().ok_or_else(|| {
                crate::errors::BridgeError::Remote("工作池中没有可用的 Worker".into())
            })?
        };
        let browser_id = worker.browser_id.read().await.clone();
        if worker.stopped.load(Ordering::Relaxed) {
            return Err(crate::errors::BridgeError::Remote(
                "浏览器实例已停止".into(),
            ));
        }
        let urls = domain
            .map(|value| {
                vec![if value.starts_with("http") {
                    value.to_owned()
                } else {
                    format!("https://{value}")
                }]
            })
            .unwrap_or_default();
        self.rpc
            .cookies_get(&browser_id, urls)
            .await
            .map(|result| json!({"instance": worker.instance_name, "cookies": result.cookies}))
    }
}

struct RuntimeExecutor<'a> {
    runtime: &'a RustRuntime,
    request: &'a GenerateRequest,
}

impl AdapterExecutor for RuntimeExecutor<'_> {
    fn execute<'a>(
        &'a self,
        worker: &'a WorkerSpec,
        adapter_type: &'a str,
        model_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<GenerationResult, String>> + Send + 'a>> {
        Box::pin(async move {
            let Some(worker) = self
                .runtime
                .workers
                .iter()
                .find(|item| item.spec.name == worker.name)
            else {
                return Err("scheduler selected an unknown worker".into());
            };
            let mut request = self.request.clone();
            request.model_id = model_id.to_owned();
            request.adapter_config = worker
                .adapters
                .get(adapter_type)
                .cloned()
                .unwrap_or(Value::Null);
            let adapter = adapter_for(adapter_type)
                .ok_or_else(|| format!("Rust adapter is not registered: {adapter_type}"))?;
            let _execution = worker.execution.lock().await;
            if worker.stopped.load(Ordering::Relaxed) || !worker.page_ready.load(Ordering::Relaxed)
            {
                return Err("browser crashed or was manually stopped".into());
            }
            let page = worker.page.read().await;
            let output = adapter
                .generate(page.as_ref(), &request)
                .await
                .map_err(|error| error.to_string())?;
            if let Some(error) = output.error {
                return Ok(GenerationResult {
                    value: None,
                    error: Some(error),
                    code: output.code,
                    retryable: output.retryable,
                });
            }
            let output_value = serde_json::to_value(output).map_err(|error| error.to_string())?;
            Ok(GenerationResult::success(output_value))
        })
    }
}

fn adapter_for(id: &str) -> Option<&'static dyn SiteAdapter> {
    match id {
        "chatgpt" => Some(&sites::ChatGptImageAdapter),
        "chatgpt_text" => Some(&sites::ChatGptTextAdapter),
        "deepseek_text" => Some(&sites::DeepSeekTextAdapter),
        "doubao" => Some(&sites::DoubaoAdapter),
        "doubao_text" => Some(&sites::DoubaoTextAdapter),
        "google_flow" => Some(&sites::GoogleFlowAdapter),
        "gemini" => Some(&sites::GeminiAdapter),
        "gemini_text" => Some(&sites::GeminiTextAdapter),
        "gemini_biz" => Some(&sites::GeminiBizAdapter),
        "gemini_biz_text" => Some(&sites::GeminiBizTextAdapter),
        "lmarena" => Some(&sites::LmArena),
        "lmarena_text" => Some(&sites::LmArenaText),
        "nanobananafree_ai" => Some(&sites::NanoBananaFreeAdapter),
        "sora" => Some(&sites::SoraAdapter),
        "zai_is" => Some(&sites::ZaiIsAdapter),
        "zai_is_text" => Some(&sites::ZaiIsTextAdapter),
        "zenmux_ai_text" => Some(&sites::ZenmuxAiTextAdapter),
        "claude_text" => Some(&crate::adapters::ClaudeTextAdapter),
        "test" => Some(&crate::adapters::TestAdapter),
        _ => None,
    }
}

fn adapter_target_url(adapter_type: &str, config: &Value) -> String {
    let adapter_config = config.pointer(&format!("/backend/adapter/{adapter_type}"));
    let configured = match adapter_type {
        "gemini_biz" | "gemini_biz_text" => adapter_config
            .and_then(|value| value.get("entryUrl"))
            .or_else(|| config.pointer("/backend/geminiBiz/entryUrl")),
        "chatgpt_text"
            if adapter_config
                .and_then(|value| value.get("temporaryChat"))
                .and_then(Value::as_bool)
                == Some(true) =>
        {
            return "https://chatgpt.com/?temporary-chat=true".into();
        }
        _ => None,
    };
    configured
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| adapter_for(adapter_type).map(|adapter| adapter.target_url().to_owned()))
        .unwrap_or_else(|| "about:blank".into())
}

async fn lock_workers(
    workers: &[RuntimeWorker],
    indexes: &[usize],
) -> Vec<tokio::sync::OwnedMutexGuard<()>> {
    let mut indexes = indexes.to_vec();
    indexes.sort_unstable();
    let mut guards = Vec::with_capacity(indexes.len());
    for index in indexes {
        guards.push(workers[index].execution.clone().lock_owned().await);
    }
    guards
}

fn select_login_worker(workers: &[Value], login: Option<&str>) -> Result<Option<String>, String> {
    let Some(login) = login else { return Ok(None) };
    let name = if login.is_empty() {
        workers
            .first()
            .and_then(|worker| worker.get("name"))
            .and_then(Value::as_str)
            .unwrap_or_default()
    } else {
        login
    };
    if workers
        .iter()
        .any(|worker| worker.get("name").and_then(Value::as_str) == Some(name))
    {
        Ok(Some(name.to_owned()))
    } else {
        let available = workers
            .iter()
            .filter_map(|worker| worker.get("name").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(", ");
        Err(format!(
            "登录模式未找到 Worker \"{name}\"。可用的 Worker: {available}"
        ))
    }
}

fn spawn_lifecycle_monitor(
    rpc: BrowserRpc,
    workers: Vec<WorkerMonitor>,
    browser_ids: Arc<tokio::sync::RwLock<Vec<String>>>,
    shutdown: Arc<AtomicBool>,
    headless: bool,
) {
    tokio::spawn(async move {
        let mut sequence = 0;
        while !shutdown.load(Ordering::Relaxed) {
            match rpc.event_poll(sequence, Some(1024)).await {
                Ok(batch) => {
                    sequence = batch.latest_sequence;
                    let closed_browsers = batch
                        .events
                        .iter()
                        .filter_map(|event| {
                            (event.fields.get("type").and_then(Value::as_str)
                                == Some("browser.closed"))
                            .then(|| {
                                event
                                    .fields
                                    .get("browserId")
                                    .and_then(Value::as_str)
                                    .map(str::to_owned)
                            })
                            .flatten()
                        })
                        .collect::<std::collections::HashSet<_>>();
                    for event in batch.events {
                        let kind = event
                            .fields
                            .get("type")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if kind == "browser.closed" {
                            let Some(browser_id) =
                                event.fields.get("browserId").and_then(Value::as_str)
                            else {
                                continue;
                            };
                            for worker in &workers {
                                if *worker.browser_id.read().await != browser_id {
                                    continue;
                                }
                                worker.page_ready.store(false, Ordering::Relaxed);
                                worker.auth_ready.store(false, Ordering::Relaxed);
                                worker.stopped.store(!headless, Ordering::Relaxed);
                            }
                            if headless {
                                let mut recovered = false;
                                for attempt in 0..4 {
                                    if recover_browser_group(
                                        &rpc,
                                        &workers,
                                        &browser_ids,
                                        browser_id,
                                    )
                                    .await
                                    .is_ok()
                                    {
                                        recovered = true;
                                        break;
                                    }
                                    tokio::time::sleep(std::time::Duration::from_millis(
                                        250 * (attempt + 1) as u64,
                                    ))
                                    .await;
                                }
                                if !recovered {
                                    for worker in &workers {
                                        if *worker.browser_id.read().await == browser_id {
                                            worker.stopped.store(true, Ordering::Relaxed);
                                        }
                                    }
                                }
                            }
                        } else if kind == "page.closed" {
                            let (Some(page_id), Some(browser_id)) = (
                                event.fields.get("pageId").and_then(Value::as_str),
                                event.fields.get("browserId").and_then(Value::as_str),
                            ) else {
                                continue;
                            };
                            if closed_browsers.contains(browser_id) {
                                continue;
                            }
                            for worker in &workers {
                                if *worker.page_id.read().await != page_id {
                                    continue;
                                }
                                worker.page_ready.store(false, Ordering::Relaxed);
                                if worker.stopped.load(Ordering::Relaxed) {
                                    continue;
                                }
                                let _guard = worker.execution.clone().lock_owned().await;
                                if shutdown.load(Ordering::Relaxed)
                                    || worker.stopped.load(Ordering::Relaxed)
                                {
                                    continue;
                                }
                                if let Ok(created) = rpc.page_create(browser_id, None, None).await {
                                    let page: Arc<dyn PageClient> = Arc::new(RpcPage::new(
                                        rpc.clone(),
                                        browser_id.to_owned(),
                                        created.page_id.clone(),
                                    ));
                                    let _ = page.call(vec![json!({"op":"goto", "url":worker.target_url, "options":{"waitUntil":"domcontentloaded", "timeout":60000}})]).await;
                                    *worker.page.write().await = page;
                                    *worker.page_id.write().await = created.page_id;
                                    worker.page_ready.store(true, Ordering::Relaxed);
                                    worker.auth_ready.store(true, Ordering::Relaxed);
                                }
                            }
                        }
                    }
                }
                Err(_) => break,
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    });
}

async fn recover_browser_group(
    rpc: &BrowserRpc,
    workers: &[WorkerMonitor],
    browser_ids: &Arc<tokio::sync::RwLock<Vec<String>>>,
    old_id: &str,
) -> Result<(), crate::errors::BridgeError> {
    let mut indexes = Vec::new();
    for (index, worker) in workers.iter().enumerate() {
        if *worker.browser_id.read().await == old_id {
            indexes.push(index);
        }
    }
    let Some(first_index) = indexes.first().copied() else {
        return Ok(());
    };
    let _guards = lock_monitor_workers(workers, &indexes).await;
    let mut still_current = false;
    for index in &indexes {
        if *workers[*index].browser_id.read().await == old_id {
            still_current = true;
            break;
        }
    }
    if !still_current {
        return Ok(());
    }
    let first = &workers[first_index];
    let started = rpc
        .browser_start(first.launch_config.clone(), first.launch_options.clone())
        .await?;
    let new_id = started.browser_id.clone();
    let mut page_ids = started.page_ids.into_iter();
    let mut replacements = Vec::with_capacity(indexes.len());
    for index in &indexes {
        let worker = &workers[*index];
        let page_id = if let Some(page_id) = page_ids.next() {
            page_id
        } else {
            match rpc.page_create(&new_id, None, None).await {
                Ok(created) => created.page_id,
                Err(error) => {
                    let _ = rpc.browser_close(&new_id).await;
                    return Err(error);
                }
            }
        };
        let page: Arc<dyn PageClient> =
            Arc::new(RpcPage::new(rpc.clone(), new_id.clone(), page_id.clone()));
        let _ = page.call(vec![json!({"op":"goto", "url":worker.target_url, "options":{"waitUntil":"domcontentloaded", "timeout":60000}})]).await;
        replacements.push((*index, page_id, page));
    }
    for (index, page_id, page) in replacements {
        let worker = &workers[index];
        *worker.page.write().await = page;
        *worker.page_id.write().await = page_id;
        *worker.browser_id.write().await = new_id.clone();
        worker.page_ready.store(true, Ordering::Relaxed);
        worker.auth_ready.store(true, Ordering::Relaxed);
        worker.stopped.store(false, Ordering::Relaxed);
    }
    let mut ids = browser_ids.write().await;
    if let Some(index) = ids.iter().position(|id| id == old_id) {
        ids[index] = new_id;
    }
    Ok(())
}

async fn lock_monitor_workers(
    workers: &[WorkerMonitor],
    indexes: &[usize],
) -> Vec<tokio::sync::OwnedMutexGuard<()>> {
    let mut indexes = indexes.to_vec();
    indexes.sort_unstable();
    let mut guards = Vec::with_capacity(indexes.len());
    for index in indexes {
        guards.push(workers[index].execution.clone().lock_owned().await);
    }
    guards
}

fn str_field<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("worker {field} is missing"))
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn adapter_request_config(config: &Value, adapter_type: &str) -> Value {
    let mut scoped = config
        .pointer(&format!("/backend/adapter/{adapter_type}"))
        .cloned()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    if let Some(object) = scoped.as_object_mut() {
        let failover = config
            .pointer("/backend/pool/failover")
            .cloned()
            .unwrap_or_else(|| json!({}));
        object.insert("failover".into(), failover.clone());
        for key in ["imgDlRetry", "imgDlRetryMaxRetries"] {
            if let Some(value) = failover.get(key) {
                object.insert(key.into(), value.clone());
            }
        }
    }
    scoped
}

fn sanitize_runtime(value: &Value) -> Value {
    match value {
        Value::Object(source) => {
            let mut result = source.clone();
            for key in ["proxyPassword", "proxyUsername", "__meta"] {
                result.remove(key);
            }
            for key in ["binaryPath", "userDataDir"] {
                if let Some(path) = result.get(key).and_then(Value::as_str) {
                    result.insert(
                        key.to_owned(),
                        Value::String(
                            std::path::Path::new(path)
                                .file_name()
                                .and_then(|name| name.to_str())
                                .unwrap_or(path)
                                .to_owned(),
                        ),
                    );
                }
            }
            for item in result.values_mut() {
                *item = sanitize_runtime(item);
            }
            Value::Object(result)
        }
        Value::Array(items) => Value::Array(items.iter().map(sanitize_runtime).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::{adapter_request_config, adapter_target_url, select_login_worker};
    use serde_json::json;

    #[test]
    fn adapter_config_keeps_site_options_and_includes_pool_failover() {
        let config = json!({
            "backend": {
                "adapter": {"lmarena": {"returnUrl": true}},
                "pool": {"failover": {"imgDlRetry": true, "imgDlRetryMaxRetries": 4, "maxRetries": 2}}
            }
        });
        let scoped = adapter_request_config(&config, "lmarena");
        assert_eq!(scoped["returnUrl"], true);
        assert_eq!(scoped["imgDlRetry"], true);
        assert_eq!(scoped["imgDlRetryMaxRetries"], 4);
        assert_eq!(scoped["failover"]["maxRetries"], 2);
    }

    #[test]
    fn login_mode_selects_only_the_requested_worker_and_reports_unknown_names() {
        let workers = vec![json!({"name":"first"}), json!({"name":"second"})];
        assert_eq!(select_login_worker(&workers, None).unwrap(), None);
        assert_eq!(
            select_login_worker(&workers, Some("")).unwrap().as_deref(),
            Some("first")
        );
        assert_eq!(
            select_login_worker(&workers, Some("second"))
                .unwrap()
                .as_deref(),
            Some("second")
        );
        assert!(select_login_worker(&workers, Some("missing"))
            .unwrap_err()
            .contains("first, second"));
    }

    #[test]
    fn initial_target_url_uses_adapter_and_business_entry_configuration() {
        assert_eq!(
            adapter_target_url("deepseek_text", &json!({})),
            "https://chat.deepseek.com/"
        );
        assert_eq!(
            adapter_target_url(
                "gemini_biz",
                &json!({"backend":{"adapter":{"gemini_biz":{"entryUrl":"https://biz.example/home"}}}})
            ),
            "https://biz.example/home"
        );
        assert_eq!(adapter_target_url("unknown", &json!({})), "about:blank");
    }
}
