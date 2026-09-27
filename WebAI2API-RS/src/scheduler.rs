//! Backend worker selection and failover policy.
//!
//! The scheduler intentionally knows nothing about the browser bridge or adapter
//! registry. Callers provide a capability snapshot and an executor that performs
//! one adapter invocation.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

static RANDOM_STATE: OnceLock<AtomicU64> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    LeastBusy,
    RoundRobin,
    Random,
}

impl Strategy {
    pub fn from_name(name: &str) -> Self {
        match name {
            "round_robin" => Self::RoundRobin,
            "random" => Self::Random,
            _ => Self::LeastBusy,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImagePolicy {
    #[default]
    Optional,
    Required,
    Forbidden,
}

impl ImagePolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Optional => "optional",
            Self::Required => "required",
            Self::Forbidden => "forbidden",
        }
    }
}

/// One model exposed by an adapter, including the metadata used by routes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCapability {
    pub id: String,
    pub model_type: String,
    pub image_policy: ImagePolicy,
}

impl ModelCapability {
    pub fn new(
        id: impl Into<String>,
        model_type: impl Into<String>,
        image_policy: ImagePolicy,
    ) -> Self {
        Self {
            id: id.into(),
            model_type: model_type.into(),
            image_policy,
        }
    }
}

/// A point-in-time adapter capability snapshot. `adapter_type` identifies the
/// registry adapter and model IDs are the unqualified IDs returned by it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterCapability {
    pub adapter_type: String,
    pub models: Vec<ModelCapability>,
}

impl AdapterCapability {
    pub fn new(adapter_type: impl Into<String>, models: Vec<ModelCapability>) -> Self {
        Self {
            adapter_type: adapter_type.into(),
            models,
        }
    }

    fn model(&self, id: &str) -> Option<&ModelCapability> {
        self.models.iter().find(|model| model.id == id)
    }
}

/// Runtime worker descriptor. Busy count is shared and atomic so concurrent
/// requests can reserve workers without serializing adapter execution.
#[derive(Debug, Clone)]
pub struct WorkerSpec {
    pub name: String,
    /// Concrete type for ordinary workers, or `"merge"` for merge workers.
    pub worker_type: String,
    pub merge_types: Vec<String>,
    pub capabilities: Vec<AdapterCapability>,
    busy_count: Arc<AtomicUsize>,
}

impl WorkerSpec {
    pub fn new(
        name: impl Into<String>,
        worker_type: impl Into<String>,
        merge_types: Vec<String>,
        capabilities: Vec<AdapterCapability>,
    ) -> Self {
        Self {
            name: name.into(),
            worker_type: worker_type.into(),
            merge_types,
            capabilities,
            busy_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn busy_count(&self) -> usize {
        self.busy_count.load(Ordering::Acquire)
    }

    fn capability(&self, adapter_type: &str) -> Option<&AdapterCapability> {
        self.capabilities
            .iter()
            .find(|capability| capability.adapter_type == adapter_type)
    }

    fn supports_adapter_model(&self, adapter_type: &str, model_id: &str) -> bool {
        self.capability(adapter_type)
            .is_some_and(|capability| capability.model(model_id).is_some())
    }

    /// Mirrors Worker.supports, including `type/model` IDs.
    pub fn supports(&self, model_id: &str) -> bool {
        if let Some((specified_type, actual_model)) = model_id.split_once('/') {
            if self.worker_type == "merge" {
                self.merge_types.iter().any(|kind| kind == specified_type)
                    && self.supports_adapter_model(specified_type, actual_model)
            } else {
                specified_type == self.worker_type
                    && self.supports_adapter_model(&self.worker_type, actual_model)
            }
        } else if self.worker_type == "merge" {
            self.merge_types
                .iter()
                .any(|kind| self.supports_adapter_model(kind, model_id))
        } else {
            self.supports_adapter_model(&self.worker_type, model_id)
        }
    }

    fn candidate_adapters(&self, model_id: &str) -> Vec<(String, String)> {
        if self.worker_type != "merge" {
            let actual = model_id
                .split_once('/')
                .map_or(model_id, |(_, model)| model);
            return if self.supports(model_id) {
                vec![(self.worker_type.clone(), actual.to_owned())]
            } else {
                Vec::new()
            };
        }
        if let Some((specified_type, actual_model)) = model_id.split_once('/') {
            return if self.merge_types.iter().any(|kind| kind == specified_type)
                && self.supports_adapter_model(specified_type, actual_model)
            {
                vec![(specified_type.to_owned(), actual_model.to_owned())]
            } else {
                Vec::new()
            };
        }
        self.merge_types
            .iter()
            .filter(|kind| self.supports_adapter_model(kind, model_id))
            .map(|kind| (kind.clone(), model_id.to_owned()))
            .collect()
    }

    fn model_info(&self, model_id: &str) -> Option<&ModelCapability> {
        if let Some((specified_type, actual_model)) = model_id.split_once('/') {
            if self.worker_type == "merge" {
                if self.merge_types.iter().any(|kind| kind == specified_type) {
                    return self.capability(specified_type)?.model(actual_model);
                }
            } else if specified_type == self.worker_type {
                return self.capability(&self.worker_type)?.model(actual_model);
            }
            return None;
        }
        if self.worker_type == "merge" {
            self.merge_types
                .iter()
                .find_map(|kind| self.capability(kind)?.model(model_id))
        } else {
            self.capability(&self.worker_type)?.model(model_id)
        }
    }

    fn image_policy(&self, model_id: &str) -> ImagePolicy {
        if let Some((specified_type, actual_model)) = model_id.split_once('/') {
            if self.worker_type == "merge"
                && self.merge_types.iter().any(|kind| kind == specified_type)
            {
                return self
                    .capability(specified_type)
                    .and_then(|capability| capability.model(actual_model))
                    .map_or(ImagePolicy::Optional, |model| model.image_policy);
            }
        }
        if self.worker_type != "merge" {
            let actual_model = model_id
                .split_once('/')
                .map_or(model_id, |(_, model)| model);
            return self
                .capability(&self.worker_type)
                .and_then(|capability| capability.model(actual_model))
                .map_or(ImagePolicy::Optional, |model| model.image_policy);
        }
        aggregate_policy(self.merge_types.iter().filter_map(|kind| {
            self.capability(kind)?
                .model(model_id)
                .map(|model| model.image_policy)
        }))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FailoverConfig {
    pub enabled: bool,
    /// `0` means try every candidate, matching the failover helper contract.
    pub max_retries: usize,
}

impl Default for FailoverConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_retries: 2,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Scheduler {
    pub strategy: Strategy,
    pub failover: FailoverConfig,
    round_robin_index: Arc<AtomicUsize>,
}

impl Scheduler {
    pub fn new(strategy: Strategy, failover: FailoverConfig) -> Self {
        Self {
            strategy,
            failover,
            round_robin_index: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Ordered supported candidates; requests with input images prefer workers
    /// whose image policy permits image input when any such worker exists.
    pub fn ordered_candidates<'a>(
        &self,
        workers: &'a [WorkerSpec],
        model_id: &str,
        has_images: bool,
    ) -> Vec<&'a WorkerSpec> {
        let mut candidates: Vec<_> = workers
            .iter()
            .filter(|worker| worker.supports(model_id))
            .collect();
        if has_images && candidates.len() > 1 {
            let image_candidates: Vec<_> = candidates
                .iter()
                .copied()
                .filter(|worker| {
                    matches!(
                        worker.image_policy(model_id),
                        ImagePolicy::Optional | ImagePolicy::Required
                    )
                })
                .collect();
            if !image_candidates.is_empty() {
                candidates = image_candidates;
            }
        }
        match self.strategy {
            Strategy::LeastBusy => candidates.sort_by_key(|worker| worker.busy_count()),
            Strategy::RoundRobin if candidates.len() > 1 => {
                let start =
                    self.round_robin_index.fetch_add(1, Ordering::Relaxed) % candidates.len();
                candidates.rotate_left(start);
            }
            Strategy::Random => shuffle(&mut candidates),
            _ => {}
        }
        candidates
    }

    /// Reserve and return the selected worker. The reservation must be released
    /// with `release` after the request finishes.
    pub fn select_worker<'a>(
        &self,
        workers: &'a [WorkerSpec],
        model_id: &str,
    ) -> Result<&'a WorkerSpec, ScheduleError> {
        self.ordered_candidates(workers, model_id, false)
            .into_iter()
            .next()
            .ok_or_else(|| ScheduleError::NoWorker(format!("没有 Worker 支持模型: {model_id}")))
            .inspect(|worker| {
                worker.busy_count.fetch_add(1, Ordering::AcqRel);
            })
    }

    pub fn release(worker: &WorkerSpec) {
        let _ = worker
            .busy_count
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |busy| {
                busy.checked_sub(1)
            });
    }

    pub fn model_type<'a>(&self, workers: &'a [WorkerSpec], model_id: &str) -> &'a str {
        workers
            .iter()
            .find(|worker| worker.supports(model_id))
            .and_then(|worker| worker.model_info(model_id))
            .map_or("image", |model| model.model_type.as_str())
    }

    /// Pool-level image policy: optional wins, then required, then forbidden.
    pub fn image_policy(&self, workers: &[WorkerSpec], model_id: &str) -> ImagePolicy {
        aggregate_policy(
            workers
                .iter()
                .filter(|worker| worker.supports(model_id))
                .map(|worker| worker.image_policy(model_id)),
        )
    }

    /// Models in original worker order. Each worker emits unqualified internal
    /// IDs followed by namespaced IDs; the pool keeps the first occurrence.
    pub fn models(&self, workers: &[WorkerSpec]) -> Vec<ListedModel> {
        let mut result = Vec::new();
        for worker in workers {
            let adapter_types: Vec<&str> = if worker.worker_type == "merge" {
                worker.merge_types.iter().map(String::as_str).collect()
            } else {
                vec![worker.worker_type.as_str()]
            };
            let mut seen_internal = Vec::<String>::new();
            for adapter_type in &adapter_types {
                if let Some(capability) = worker.capability(adapter_type) {
                    for model in &capability.models {
                        if !seen_internal.contains(&model.id) {
                            seen_internal.push(model.id.clone());
                            result.push(ListedModel::from_capability(
                                model,
                                "internal_server",
                                model.id.clone(),
                            ));
                        }
                    }
                }
            }
            for adapter_type in adapter_types {
                if let Some(capability) = worker.capability(adapter_type) {
                    for model in &capability.models {
                        result.push(ListedModel::from_capability(
                            model,
                            adapter_type,
                            format!("{adapter_type}/{}", model.id),
                        ));
                    }
                }
            }
        }
        let mut seen = Vec::<String>::new();
        result.retain(|model| {
            if seen.contains(&model.id) {
                false
            } else {
                seen.push(model.id.clone());
                true
            }
        });
        result
    }

    /// Execute candidates sequentially with Node-compatible failover behavior.
    /// Merge workers try their eligible adapters in configured mergeTypes order.
    pub async fn execute<E: AdapterExecutor>(
        &self,
        workers: &[WorkerSpec],
        model_id: &str,
        has_images: bool,
        executor: &E,
    ) -> GenerationResult {
        let candidates = self.ordered_candidates(workers, model_id, has_images);
        if candidates.is_empty() {
            return GenerationResult::error(format!("没有 Worker 支持模型: {model_id}"));
        }
        let attempt_limit = attempts(self.failover.max_retries, candidates.len());
        let mut last_error = None;
        for worker in candidates.into_iter().take(attempt_limit) {
            let adapters = worker.candidate_adapters(model_id);
            let worker_outcome = if worker.worker_type == "merge" && self.failover.enabled {
                self.execute_merge(worker, adapters, model_id, executor)
                    .await
            } else {
                let Some((adapter_type, actual_model)) = adapters.into_iter().next() else {
                    return GenerationResult::error(format!(
                        "Worker [{}] 不支持模型: {model_id}",
                        worker.name
                    ));
                };
                self.execute_reserved(worker, &adapter_type, &actual_model, executor)
                    .await
            };
            if worker_outcome.error.is_none() {
                return worker_outcome;
            }
            let error = worker_outcome.error.clone().unwrap_or_default();
            let retryable = worker_outcome
                .retryable
                .unwrap_or_else(|| classify_retryable(&error));
            if !self.failover.enabled || !retryable {
                return if retryable {
                    worker_outcome
                } else {
                    GenerationResult {
                        error: Some(error),
                        code: Some("NOT_RETRYABLE".into()),
                        retryable: Some(false),
                        value: None,
                    }
                };
            }
            last_error = Some(error);
        }
        GenerationResult {
            error: Some(format!(
                "所有候选都失败: {}",
                last_error.unwrap_or_default()
            )),
            code: Some("FAILOVER_EXHAUSTED".into()),
            retryable: Some(false),
            value: None,
        }
    }

    async fn execute_merge<E: AdapterExecutor>(
        &self,
        worker: &WorkerSpec,
        adapters: Vec<(String, String)>,
        model_id: &str,
        executor: &E,
    ) -> GenerationResult {
        if adapters.is_empty() {
            return GenerationResult::error(format!(
                "Worker [{}] 不支持模型: {model_id}",
                worker.name
            ));
        }
        let limit = attempts(self.failover.max_retries, adapters.len());
        let mut last_error = None;
        let mut last_retryable = None;
        for (adapter_type, actual_model) in adapters.into_iter().take(limit) {
            let result = self
                .execute_reserved(worker, &adapter_type, &actual_model, executor)
                .await;
            if result.error.is_none() {
                return result;
            }
            let error = result.error.clone().unwrap_or_default();
            let retryable = result
                .retryable
                .unwrap_or_else(|| classify_retryable(&error));
            if !retryable {
                return GenerationResult {
                    error: Some(format!("所有支持该模型的适配器都无法使用: {error}")),
                    code: result.code,
                    retryable: Some(false),
                    value: None,
                };
            }
            last_error = Some(error);
            last_retryable = Some(retryable);
        }
        GenerationResult {
            error: Some(format!(
                "所有支持该模型的适配器都无法使用: {}",
                last_error.unwrap_or_default()
            )),
            code: None,
            retryable: last_retryable,
            value: None,
        }
    }

    async fn execute_reserved<E: AdapterExecutor>(
        &self,
        worker: &WorkerSpec,
        adapter_type: &str,
        model_id: &str,
        executor: &E,
    ) -> GenerationResult {
        worker.busy_count.fetch_add(1, Ordering::AcqRel);
        let _reservation = Reservation(worker);
        match executor.execute(worker, adapter_type, model_id).await {
            Ok(result) => result,
            Err(message) => GenerationResult {
                error: Some(message.clone()),
                code: None,
                retryable: Some(classify_retryable(&message)),
                value: None,
            },
        }
    }
}

fn attempts(max_retries: usize, candidate_count: usize) -> usize {
    if max_retries == 0 {
        candidate_count
    } else {
        (max_retries + 1).min(candidate_count)
    }
}

fn aggregate_policy(policies: impl IntoIterator<Item = ImagePolicy>) -> ImagePolicy {
    let policies: Vec<_> = policies.into_iter().collect();
    if policies.contains(&ImagePolicy::Optional) {
        ImagePolicy::Optional
    } else if policies.contains(&ImagePolicy::Required) {
        ImagePolicy::Required
    } else if policies.contains(&ImagePolicy::Forbidden) {
        ImagePolicy::Forbidden
    } else {
        ImagePolicy::Optional
    }
}

fn shuffle<T>(items: &mut [T]) {
    let state_cell = RANDOM_STATE.get_or_init(|| {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x9e37_79b9_7f4a_7c15, |duration| duration.as_nanos() as u64);
        AtomicU64::new(seed.max(1))
    });
    for index in (1..items.len()).rev() {
        let mut state = state_cell.load(Ordering::Relaxed);
        loop {
            let mut next = state;
            next ^= next << 13;
            next ^= next >> 7;
            next ^= next << 17;
            match state_cell.compare_exchange_weak(
                state,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    state = next;
                    break;
                }
                Err(current) => state = current,
            }
        }
        items.swap(index, (state as usize) % (index + 1));
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListedModel {
    pub id: String,
    pub object: String,
    pub owned_by: String,
    pub model_type: String,
    pub image_policy: ImagePolicy,
}

impl ListedModel {
    fn from_capability(model: &ModelCapability, owner: &str, id: String) -> Self {
        Self {
            id,
            object: "model".into(),
            owned_by: owner.into(),
            model_type: model.model_type.clone(),
            image_policy: model.image_policy,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GenerationResult {
    pub value: Option<Value>,
    pub error: Option<String>,
    pub code: Option<String>,
    pub retryable: Option<bool>,
}

impl GenerationResult {
    pub fn success(value: Value) -> Self {
        Self {
            value: Some(value),
            error: None,
            code: None,
            retryable: None,
        }
    }
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            value: None,
            error: Some(message.into()),
            code: None,
            retryable: None,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ScheduleError {
    #[error("{0}")]
    NoWorker(String),
}

pub trait AdapterExecutor: Sync {
    fn execute<'a>(
        &'a self,
        worker: &'a WorkerSpec,
        adapter_type: &'a str,
        model_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<GenerationResult, String>> + Send + 'a>>;
}

struct Reservation<'a>(&'a WorkerSpec);
impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        Scheduler::release(self.0);
    }
}

/// Mirrors utils/error.js retryable patterns for errors that carry no explicit
/// `retryable` bit. Content blocks, client errors and captchas stay nonretryable.
pub fn classify_retryable(message: &str) -> bool {
    let lower = message.to_lowercase();
    [
        "network",
        "net::",
        "econnreset",
        "econnrefused",
        "etimedout",
        "timeout",
        "timed out",
        "加载超时",
        "请求超时",
        "crashed",
        "crash",
        "500",
        "501",
        "502",
        "503",
        "504",
        "internal server error",
        "bad gateway",
        "service unavailable",
        "rate limit",
        "too many requests",
        "429",
    ]
    .iter()
    .any(|pattern| lower.contains(pattern))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Barrier;

    fn capability(kind: &str, entries: &[(&str, &str, ImagePolicy)]) -> AdapterCapability {
        AdapterCapability::new(
            kind,
            entries
                .iter()
                .map(|(id, model_type, policy)| ModelCapability::new(*id, *model_type, *policy))
                .collect(),
        )
    }
    fn worker(name: &str, kind: &str, entries: &[(&str, &str, ImagePolicy)]) -> WorkerSpec {
        WorkerSpec::new(name, kind, vec![], vec![capability(kind, entries)])
    }
    fn merge(name: &str, order: &[&str]) -> WorkerSpec {
        let mut caps = Vec::new();
        for kind in order {
            caps.push(capability(
                kind,
                &[("shared", "image", ImagePolicy::Optional)],
            ));
        }
        WorkerSpec::new(
            name,
            "merge",
            order.iter().map(|s| s.to_string()).collect(),
            caps,
        )
    }

    struct SequenceExecutor {
        results: Vec<GenerationResult>,
        calls: AtomicUsize,
    }
    impl AdapterExecutor for SequenceExecutor {
        fn execute<'a>(
            &'a self,
            _worker: &'a WorkerSpec,
            _adapter_type: &'a str,
            _model_id: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<GenerationResult, String>> + Send + 'a>> {
            Box::pin(async move {
                let index = self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(self
                    .results
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| GenerationResult::success(Value::Null)))
            })
        }
    }

    #[test]
    fn support_and_image_input_priority_match_worker_rules() {
        let forbidden = worker("a", "one", &[("m", "image", ImagePolicy::Forbidden)]);
        let optional = worker("b", "two", &[("m", "image", ImagePolicy::Optional)]);
        let scheduler = Scheduler::new(Strategy::LeastBusy, FailoverConfig::default());
        assert!(forbidden.supports("m"));
        assert!(optional.supports("two/m"));
        assert!(!forbidden.supports("two/m"));
        assert_eq!(
            scheduler.ordered_candidates(&[forbidden.clone(), optional.clone()], "m", true)[0].name,
            "b"
        );
        assert_eq!(
            scheduler.image_policy(&[forbidden, optional], "m"),
            ImagePolicy::Optional
        );
    }

    #[test]
    fn merge_namespaced_type_and_metadata_precedence() {
        let worker = WorkerSpec::new(
            "merged",
            "merge",
            vec!["one".into(), "two".into()],
            vec![
                capability("one", &[("m", "text", ImagePolicy::Forbidden)]),
                capability("two", &[("m", "image", ImagePolicy::Required)]),
            ],
        );
        let scheduler = Scheduler::new(Strategy::LeastBusy, FailoverConfig::default());
        assert!(worker.supports("m"));
        assert!(worker.supports("two/m"));
        assert!(!worker.supports("missing/m"));
        assert_eq!(
            scheduler.model_type(std::slice::from_ref(&worker), "m"),
            "text"
        );
        assert_eq!(
            scheduler.model_type(std::slice::from_ref(&worker), "two/m"),
            "image"
        );
        assert_eq!(
            scheduler.image_policy(std::slice::from_ref(&worker), "m"),
            ImagePolicy::Required
        );
    }

    #[test]
    fn ordering_strategies_and_atomic_reservations() {
        let workers = vec![
            worker("a", "one", &[("m", "image", ImagePolicy::Optional)]),
            worker("b", "one", &[("m", "image", ImagePolicy::Optional)]),
        ];
        workers[0].busy_count.store(2, Ordering::Release);
        let least = Scheduler::new(Strategy::LeastBusy, FailoverConfig::default());
        assert_eq!(least.select_worker(&workers, "m").unwrap().name, "b");
        Scheduler::release(&workers[1]);
        let rr = Scheduler::new(Strategy::RoundRobin, FailoverConfig::default());
        assert_eq!(rr.ordered_candidates(&workers, "m", false)[0].name, "a");
        assert_eq!(rr.ordered_candidates(&workers, "m", false)[0].name, "b");
        let random = Scheduler::new(Strategy::Random, FailoverConfig::default());
        for _ in 0..30 {
            assert_eq!(random.ordered_candidates(&workers, "m", false).len(), 2);
        }
    }

    #[tokio::test]
    async fn retries_count_zero_means_all_and_nonretryable_stops() {
        let workers = vec![
            worker("a", "one", &[("m", "image", ImagePolicy::Optional)]),
            worker("b", "one", &[("m", "image", ImagePolicy::Optional)]),
        ];
        let scheduler = Scheduler::new(
            Strategy::RoundRobin,
            FailoverConfig {
                enabled: true,
                max_retries: 0,
            },
        );
        let executor = SequenceExecutor {
            results: vec![
                GenerationResult {
                    error: Some("temporary network error".into()),
                    code: None,
                    retryable: Some(true),
                    value: None,
                },
                GenerationResult::success(serde_json::json!({"ok": true})),
            ],
            calls: AtomicUsize::new(0),
        };
        assert!(scheduler
            .execute(&workers, "m", false, &executor)
            .await
            .error
            .is_none());
        assert_eq!(executor.calls.load(Ordering::SeqCst), 2);

        let stopped = SequenceExecutor {
            results: vec![GenerationResult {
                error: Some("blocked".into()),
                code: None,
                retryable: Some(false),
                value: None,
            }],
            calls: AtomicUsize::new(0),
        };
        let result = scheduler.execute(&workers, "m", false, &stopped).await;
        assert_eq!(result.error.as_deref(), Some("blocked"));
        assert_eq!(result.code.as_deref(), Some("NOT_RETRYABLE"));
        assert_eq!(stopped.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn merge_adapter_fallback_and_model_union() {
        let merged = merge("merged", &["one", "two"]);
        let scheduler = Scheduler::new(
            Strategy::LeastBusy,
            FailoverConfig {
                enabled: true,
                max_retries: 0,
            },
        );
        let executor = SequenceExecutor {
            results: vec![
                GenerationResult {
                    error: Some("upstream 503".into()),
                    code: Some("HTTP_ERROR".into()),
                    retryable: Some(true),
                    value: None,
                },
                GenerationResult::success(Value::Null),
            ],
            calls: AtomicUsize::new(0),
        };
        assert!(scheduler
            .execute(std::slice::from_ref(&merged), "shared", false, &executor)
            .await
            .error
            .is_none());
        assert_eq!(executor.calls.load(Ordering::SeqCst), 2);
        let models = scheduler.models(&[merged]);
        assert_eq!(
            models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec!["shared", "one/shared", "two/shared"]
        );
        assert_eq!(models[0].owned_by, "internal_server");
    }

    #[tokio::test]
    async fn concurrent_executions_reserve_and_release_busy_counts() {
        let workers = vec![
            worker("a", "one", &[("m", "image", ImagePolicy::Optional)]),
            worker("b", "one", &[("m", "image", ImagePolicy::Optional)]),
        ];
        let barrier = Arc::new(Barrier::new(2));
        struct ConcurrentExecutor(Arc<Barrier>);
        impl AdapterExecutor for ConcurrentExecutor {
            fn execute<'a>(
                &'a self,
                worker: &'a WorkerSpec,
                _: &'a str,
                _: &'a str,
            ) -> Pin<Box<dyn Future<Output = Result<GenerationResult, String>> + Send + 'a>>
            {
                Box::pin(async move {
                    self.0.wait().await;
                    Ok(GenerationResult::success(Value::String(
                        worker.name.clone(),
                    )))
                })
            }
        }
        let scheduler = Scheduler::new(
            Strategy::LeastBusy,
            FailoverConfig {
                enabled: false,
                max_retries: 2,
            },
        );
        let executor = ConcurrentExecutor(barrier);
        let (left, right) = tokio::join!(
            scheduler.execute(&workers, "m", false, &executor),
            scheduler.execute(&workers, "m", false, &executor)
        );
        assert_ne!(
            left.value, right.value,
            "atomic reservations should distribute simultaneous work"
        );
        assert_eq!(
            workers
                .iter()
                .map(WorkerSpec::busy_count)
                .collect::<Vec<_>>(),
            vec![0, 0]
        );
    }

    #[test]
    fn error_classifier_tracks_retryable_js_patterns() {
        assert!(classify_retryable("ETIMEDOUT"));
        assert!(classify_retryable("上游服务器错误 HTTP 503"));
        assert!(classify_retryable("429 Too Many Requests"));
        assert!(!classify_retryable("内容被拒绝"));
        assert!(!classify_retryable("触发人机验证"));
    }
}
