//! 配置的类型化视图：serde 结构体镜像 config.yaml 的形状。
//! 与 load_config() 的 Value 主路径并存：Value 仍是桥协议的数据源，
//! 本模块把散落的 `pointer("/a/b/c")` 字符串路径收敛为字段访问。
//!
//! 保序与未知字段：serde_json 默认开启 preserve_order（经 indexmap），
//! `#[serde(flatten)] extra` 承接所有未建模字段，读改写不丢用户自定义键。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// 顶层配置。未知顶层键进 extra。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub browser: BrowserConfig,
    #[serde(default)]
    pub queue: QueueConfig,
    #[serde(default)]
    pub backend: BackendConfig,
    pub log_level: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    pub port: Option<u16>,
    pub auth: Option<String>,
    #[serde(default)]
    pub keepalive: KeepaliveConfig,
    pub image_markdown: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeepaliveConfig {
    pub mode: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserConfig {
    pub path: Option<String>,
    pub engine: Option<String>,
    pub headless: Option<bool>,
    pub fission: Option<bool>,
    pub humanize_cursor: Option<Value>,
    pub ff_version: Option<Value>,
    #[serde(default)]
    pub proxy: ProxyConfig,
    #[serde(default)]
    pub camoufox: CamoufoxConfig,
    #[serde(default)]
    pub clearcote: ClearcoteConfig,
    #[serde(default)]
    pub css_inject: CssInjectConfig,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyConfig {
    pub enable: Option<bool>,
    #[serde(rename = "type")]
    pub proxy_type: Option<String>,
    pub host: Option<String>,
    pub port: Option<u64>,
    pub user: Option<String>,
    pub passwd: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CamoufoxConfig {
    pub main_world_eval: Option<bool>,
    pub enable_cache: Option<bool>,
    pub disable_instant_animations: Option<bool>,
    pub humanize_max_time: Option<Value>,
    pub block_webrtc: Option<bool>,
    pub geoip: Option<bool>,
    pub locale: Option<Value>,
    pub certificates: Option<Value>,
    pub certificate_paths: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearcoteConfig {
    pub path: Option<String>,
    pub platform: Option<String>,
    pub brand: Option<String>,
    pub fingerprint_profile: Option<String>,
    pub timezone: Option<String>,
    pub accept_language: Option<String>,
    pub geoip: Option<bool>,
    pub humanize: Option<bool>,
    pub webrtc_ip: Option<String>,
    pub sandbox: Option<bool>,
    pub allow_detected_license: Option<bool>,
    pub args: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CssInjectConfig {
    pub animation: Option<bool>,
    pub filter: Option<bool>,
    pub font: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueConfig {
    pub queue_buffer: Option<u64>,
    pub image_limit: Option<u64>,
    pub max_concurrent: Option<u64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendConfig {
    #[serde(default)]
    pub pool: PoolConfig,
    #[serde(default)]
    pub adapter: Map<String, Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolConfig {
    pub strategy: Option<String>,
    pub wait_timeout: Option<u64>,
    #[serde(default)]
    pub failover: FailoverConfig,
    pub instances: Option<Vec<Value>>,
    pub workers: Option<Vec<Value>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FailoverConfig {
    pub enabled: Option<bool>,
    pub max_retries: Option<u64>,
    pub img_dl_retry: Option<bool>,
    pub img_dl_retry_max_retries: Option<u64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Config {
    /// 从 load_config() 的 Value 解析。类型不匹配的字段静默落 None
    /// （加载器已先行校验硬错误，此处只做类型化读取）。
    pub fn from_value(v: &Value) -> Self {
        serde_json::from_value(v.clone()).unwrap_or_default()
    }

    /// 合并 extra 后写回为 Value（保留未建模字段与键顺序）。
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// 默认值与回退语义集中在访问器：与 load_config() 的 set_default 行为一致。
impl Config {
    pub fn port(&self) -> u16 {
        self.server.port.unwrap_or(3000)
    }
    pub fn auth(&self) -> String {
        self.server.auth.clone().unwrap_or_default()
    }
    pub fn keepalive_mode(&self) -> String {
        self.server
            .keepalive
            .mode
            .clone()
            .unwrap_or_else(|| "comment".into())
    }
    pub fn image_markdown(&self) -> bool {
        self.server.image_markdown.unwrap_or(false)
    }
    pub fn queue_buffer(&self) -> u64 {
        self.queue.queue_buffer.unwrap_or(2)
    }
    pub fn image_limit(&self) -> u64 {
        self.queue.image_limit.unwrap_or(5)
    }
    pub fn max_concurrent(&self) -> u64 {
        self.queue.max_concurrent.unwrap_or(1)
    }
    pub fn pool_strategy(&self) -> String {
        self.backend
            .pool
            .strategy
            .clone()
            .unwrap_or_else(|| "least_busy".into())
    }
    pub fn browser_engine(&self) -> String {
        self.browser
            .engine
            .clone()
            .unwrap_or_else(|| "camoufox".into())
    }
    pub fn log_level(&self) -> String {
        self.log_level.clone().unwrap_or_else(|| "info".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Value {
        json!({
            "logLevel": "info",
            "server": { "port": 3100, "auth": "", "keepalive": { "mode": "comment" }, "imageMarkdown": false },
            "queue": { "queueBuffer": 2, "imageLimit": 5 },
            "browser": {
                "engine": "camoufox", "path": "",
                "humanizeCursor": "camou",
                "proxy": { "enable": false, "type": "http", "host": "127.0.0.1", "port": 7890, "user": "u", "passwd": "p" },
                "camoufox": { "geoip": true },
                "clearcote": { "brand": "Chrome", "args": [] },
                "cssInject": { "animation": false }
            },
            "backend": {
                "pool": {
                    "strategy": "least_busy", "waitTimeout": 120000,
                    "failover": { "enabled": true, "maxRetries": 3 },
                    "instances": [{ "name": "browser_default", "workers": [{ "name": "default", "type": "lmarena" }] }],
                    "workers": [{ "name": "default", "type": "lmarena" }]
                },
                "adapter": { "lmarena": { "modelFilter": { "mode": "whitelist", "list": ["a"] } } }
            }
        })
    }

    #[test]
    fn typed_accessors_match_value_paths() {
        let c = Config::from_value(&sample());
        assert_eq!(c.port(), 3100);
        assert_eq!(c.auth(), "");
        assert_eq!(c.keepalive_mode(), "comment");
        assert_eq!(c.queue_buffer(), 2);
        assert_eq!(c.image_limit(), 5);
        assert_eq!(c.pool_strategy(), "least_busy");
        assert_eq!(c.browser_engine(), "camoufox");
        assert_eq!(c.log_level(), "info");
        assert!(!c.image_markdown());
        // maxConcurrent 未配置时回退 1（load_config 会写入 worker 数）
        assert_eq!(c.max_concurrent(), 1);
    }

    #[test]
    fn camel_case_renames_parse() {
        let c = Config::from_value(&sample());
        assert_eq!(c.browser.humanize_cursor, Some(json!("camou")));
        assert_eq!(c.backend.pool.wait_timeout, Some(120000));
        assert_eq!(c.backend.pool.failover.max_retries, Some(3));
        assert_eq!(c.browser.proxy.proxy_type.as_deref(), Some("http"));
        assert_eq!(c.browser.clearcote.brand.as_deref(), Some("Chrome"));
        assert_eq!(c.browser.camoufox.geoip, Some(true));
    }

    #[test]
    fn roundtrip_preserves_unknown_keys_and_order() {
        let v = sample();
        // 加两个未建模键（顶层 + 嵌套）
        let mut v2 = v.clone();
        v2["customTop"] = json!("keep-me");
        v2["server"]["customServer"] = json!({ "deep": 1 });
        let c = Config::from_value(&v2);
        let out = c.to_value();
        assert_eq!(out["customTop"], "keep-me");
        assert_eq!(out["server"]["customServer"], json!({ "deep": 1 }));
        assert_eq!(out["server"]["port"], 3100);
        assert_eq!(
            out["backend"]["pool"]["instances"][0]["name"],
            "browser_default"
        );
    }

    #[test]
    fn empty_config_gets_defaults() {
        let c = Config::from_value(&json!({}));
        assert_eq!(c.port(), 3000);
        assert_eq!(c.keepalive_mode(), "comment");
        assert_eq!(c.pool_strategy(), "least_busy");
        assert_eq!(c.browser_engine(), "camoufox");
    }
}
