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
use std::pin::Pin;
use std::sync::Arc;

struct RuntimeWorker {
    spec: WorkerSpec,
    page: Arc<dyn PageClient>,
    adapters: HashMap<String, Value>,
}

/// Initialized browser handles and Rust-owned scheduler/adapter state.
pub struct RustRuntime {
    rpc: BrowserRpc,
    config: Value,
    workers: Vec<RuntimeWorker>,
    scheduler: Scheduler,
    browser_ids: Vec<String>,
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
            Self::Rust(_) => Err(crate::errors::BridgeError::Remote(
                "browser restart is not yet wired for the Rust runtime".into(),
            )),
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
            Self::Rust(_) => Err(crate::errors::BridgeError::Remote(
                "context download is not yet wired for the Rust runtime".into(),
            )),
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
    login: bool,
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
        .env("WEBAI2API_LOGIN", if login { "1" } else { "" })
        .env("WEBAI2API_TEMP_DIR", temp_dir)
        .env("CAMOUFOX_INSTALL_DIR", src_root.join("camoufox"))
        .current_dir(&cwd)
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true);
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
    Ok((child, rpc))
}

impl RustRuntime {
    pub fn empty(rpc: BrowserRpc, config: Value) -> Self {
        Self {
            rpc,
            config,
            workers: Vec::new(),
            scheduler: Scheduler::new(Strategy::LeastBusy, FailoverConfig::default()),
            browser_ids: Vec::new(),
        }
    }
    /// Start configured browser instances and materialize the worker capability
    /// snapshot from the embedded Rust catalog.
    pub async fn initialize(rpc: BrowserRpc, config: Value) -> Result<Self, String> {
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
            let page: Arc<dyn PageClient> =
                Arc::new(RpcPage::new(rpc.clone(), browser.browser_id, page_id));
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
                    config
                        .pointer(&format!("/backend/adapter/{adapter_type}"))
                        .cloned()
                        .unwrap_or_else(|| json!({})),
                );
            }
            workers.push(RuntimeWorker {
                spec: WorkerSpec::new(worker_name, worker_type, merge_types, capabilities),
                page,
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
            browser_ids,
        })
    }

    pub fn worker_snapshot(&self) -> Value {
        json!({"workers": self.workers.iter().map(|worker| json!({
            "name": worker.spec.name,
            "type": worker.spec.worker_type,
            "busy": worker.spec.busy_count(),
        })).collect::<Vec<_>>()})
    }

    pub fn models(&self) -> Value {
        let models = self.scheduler.models(
            &self
                .workers
                .iter()
                .map(|worker| worker.spec.clone())
                .collect::<Vec<_>>(),
        );
        json!({"object":"list", "data": models})
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

    pub fn browser_ids(&self) -> &[String] {
        &self.browser_ids
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
        for id in &self.browser_ids {
            let _ = self.rpc.browser_close(id).await;
        }
    }

    async fn cookies(
        &self,
        instance: Option<&str>,
        domain: Option<&str>,
    ) -> Result<Value, crate::errors::BridgeError> {
        let _ = instance;
        let browser_id = self
            .browser_ids
            .first()
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
            .cookies_get(browser_id, urls)
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
            let output = adapter
                .generate(worker.page.as_ref(), &request)
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
