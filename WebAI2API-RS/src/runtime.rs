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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

struct RuntimeWorker {
    spec: WorkerSpec,
    page: tokio::sync::RwLock<Arc<dyn PageClient>>,
    page_id: tokio::sync::RwLock<String>,
    browser_id: tokio::sync::RwLock<String>,
    instance_name: String,
    engine: String,
    user_data_dir: String,
    browser_runtime: Option<Value>,
    execution: tokio::sync::Mutex<()>,
    adapters: HashMap<String, Value>,
}

/// Initialized browser handles and Rust-owned scheduler/adapter state.
pub struct RustRuntime {
    rpc: BrowserRpc,
    config: Value,
    workers: Vec<RuntimeWorker>,
    scheduler: Scheduler,
    browser_ids: tokio::sync::RwLock<Vec<String>>,
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
            Self::Rust(_) => Ok(()),
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
            browser_ids: tokio::sync::RwLock::new(Vec::new()),
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
        let workers_config = config
            .pointer("/backend/pool/workers")
            .and_then(Value::as_array)
            .ok_or_else(|| "backend.pool.workers is missing".to_string())?;
        let mut browsers: HashMap<String, BrowserStarted> = HashMap::new();
        let mut assigned_pages = std::collections::HashSet::new();
        let mut browser_ids = Vec::new();
        let mut workers = Vec::new();

        for worker_cfg in workers_config {
            let worker_name = str_field(worker_cfg, "name")?;
            let worker_type = str_field(worker_cfg, "type")?;
            let merge_types = string_array(worker_cfg.get("mergeTypes"));
            let engine = worker_cfg
                .get("engine")
                .and_then(Value::as_str)
                .unwrap_or("camoufox");
            let user_data_dir = worker_cfg
                .get("userDataDir")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let browser_key = format!("{engine}\0{user_data_dir}");
            let browser = if let Some(browser) = browsers.get(&browser_key) {
                browser.clone()
            } else {
                let options = json!({
                    "engine": engine,
                    "userDataDir": user_data_dir,
                    "instanceName": worker_cfg.get("instanceName").and_then(Value::as_str),
                    "proxyConfig": worker_cfg.get("resolvedProxy").cloned().unwrap_or(Value::Null),
                });
                let started = rpc
                    .browser_start(config.clone(), options)
                    .await
                    .map_err(|error| error.to_string())?;
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
                rpc.page_create(&browser.browser_id, None, None)
                    .await
                    .map_err(|error| error.to_string())?
                    .page_id
            };
            let page: Arc<dyn PageClient> = Arc::new(RpcPage::new(
                rpc.clone(),
                browser.browser_id.clone(),
                page_id.clone(),
            ));
            let adapter_types = if worker_type == "merge" {
                merge_types.clone()
            } else {
                vec![worker_type.to_owned()]
            };
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
            workers.push(RuntimeWorker {
                spec: WorkerSpec::new(worker_name, worker_type, merge_types, capabilities),
                page: tokio::sync::RwLock::new(page),
                page_id: tokio::sync::RwLock::new(page_id),
                browser_id: tokio::sync::RwLock::new(browser.browser_id.clone()),
                instance_name: worker_cfg
                    .get("instanceName")
                    .and_then(Value::as_str)
                    .unwrap_or(worker_name)
                    .to_owned(),
                engine: engine.to_owned(),
                user_data_dir: user_data_dir.to_owned(),
                browser_runtime: browser.runtime.clone(),
                execution: tokio::sync::Mutex::new(()),
                adapters: adapter_configs,
            });
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
        Ok(Self {
            rpc,
            config,
            workers,
            scheduler,
            browser_ids: tokio::sync::RwLock::new(browser_ids),
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
            "pageReady": true,
            "authReady": true,
            "mergeTypes": worker.spec.merge_types,
            "stopped": false,
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
        for id in self.browser_ids.read().await.iter() {
            let _ = self.rpc.browser_close(id).await;
        }
        let _ = self.rpc.shutdown().await;
    }

    async fn restart_browser(&self) -> Result<Value, crate::errors::BridgeError> {
        let old_ids = self.browser_ids.read().await.clone();
        let mut replacement_ids = old_ids.clone();
        for (index, old_id) in old_ids.iter().enumerate() {
            let restarted = self.rpc.browser_restart(old_id).await?;
            let browser = restarted.started;
            let new_id = browser.browser_id.clone();
            let mut pages = browser.page_ids.into_iter();
            for worker in &self.workers {
                if *worker.browser_id.read().await != *old_id {
                    continue;
                }
                let page_id = if let Some(page_id) = pages.next() {
                    page_id
                } else {
                    self.rpc.page_create(&new_id, None, None).await?.page_id
                };
                *worker.page.write().await = Arc::new(RpcPage::new(
                    self.rpc.clone(),
                    new_id.clone(),
                    page_id.clone(),
                ));
                *worker.page_id.write().await = page_id;
                *worker.browser_id.write().await = new_id.clone();
            }
            replacement_ids[index] = new_id;
        }
        *self.browser_ids.write().await = replacement_ids;
        Ok(json!({
            "browserStopped": false,
            "workers": self.worker_snapshot()["workers"].clone(),
            "result": {"success": true, "message": "浏览器已重启"}
        }))
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
        let mut browser_id = None;
        for worker in &self.workers {
            if instance.is_none_or(|name| worker.instance_name == name) {
                browser_id = Some(worker.browser_id.read().await.clone());
                break;
            }
        }
        let browser_id = browser_id
            .ok_or_else(|| crate::errors::BridgeError::Remote("browser is unavailable".into()))?;
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
            .map(|result| json!({"cookies": result.cookies}))
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
    use super::adapter_request_config;
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
}
