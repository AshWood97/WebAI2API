//! 配置加载与校验，语义对应原 `src/config/index.js` + `engineContract.js` 的纯函数部分。
//! 两类错误语义保持不变：非法枚举值"警告并回退"（keepalive.mode、pool.strategy），
//! 硬错误直接返回 Err（端口、instances、engine、clearcote.platform、gemini_biz.entryUrl）。

use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

pub const DEFAULT_ENGINE: &str = "camoufox";
const ENGINES: [&str; 2] = ["camoufox", "clearcote"];
const MARK_RE_OK: fn(char) -> bool = |c| c.is_ascii_alphanumeric() || c == '_' || c == '-';

/// clearcote.args 禁止覆盖的参数前缀（engineContract.js sanitizeClearcoteArgs）。
const FORBIDDEN_ARG_PREFIXES: [&str; 8] = [
    "--user-data-dir",
    "--proxy-server",
    "--proxy-bypass-list",
    "--fingerprint",
    "--fingerprint-profile",
    "--remote-debugging-port",
    "--remote-debugging-pipe",
    "--headless",
];

#[derive(Debug)]
pub struct ConfigError(pub String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 解析配置路径：data/config.yaml > 根目录 config.yaml（自动迁移）> 从 example 复制。
pub fn resolve_config_path(root: &Path) -> Result<PathBuf, ConfigError> {
    resolve_config_path_in(root, &root.join("data"))
}

pub fn resolve_config_path_in(root: &Path, data_dir: &Path) -> Result<PathBuf, ConfigError> {
    let data_cfg = data_dir.join("config.yaml");
    if data_cfg.exists() {
        return Ok(data_cfg);
    }
    let root_cfg = root.join("config.yaml");
    if root_cfg.exists() {
        fs::create_dir_all(data_dir).ok();
        fs::rename(&root_cfg, &data_cfg)
            .map_err(|e| ConfigError(format!("迁移 config.yaml 失败: {e}")))?;
        crate::logfmt::info("配置器", "已将 config.yaml 迁移到 data/config.yaml");
        return Ok(data_cfg);
    }
    let example = root.join("config.example.yaml");
    if example.exists() {
        fs::create_dir_all(data_dir).map_err(|e| ConfigError(format!("创建数据目录失败: {e}")))?;
        fs::copy(&example, &data_cfg).map_err(|e| ConfigError(format!("复制示例配置失败: {e}")))?;
        crate::logfmt::info("配置器", "已从 config.example.yaml 生成 data/config.yaml");
        return Ok(data_cfg);
    }
    Err(ConfigError(format!(
        "未找到配置文件: {}。请确保 data/config.yaml、config.yaml 或 config.example.yaml 存在。",
        data_cfg.display()
    )))
}

pub fn normalize_engine(value: Option<&str>) -> Result<String, ConfigError> {
    match value.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(DEFAULT_ENGINE.to_string()),
        Some(s) => {
            let e = s.to_ascii_lowercase();
            if ENGINES.contains(&e.as_str()) {
                Ok(e)
            } else {
                Err(ConfigError(format!(
                    "未知 browser engine: {s}（允许: camoufox | clearcote）"
                )))
            }
        }
    }
}

fn validate_user_data_mark(mark: Option<&str>) -> Result<(), ConfigError> {
    let Some(mark) = mark.filter(|s| !s.is_empty()) else {
        return Ok(());
    };
    if mark != mark.trim() || !mark.chars().all(MARK_RE_OK) {
        return Err(ConfigError(format!(
            "userDataMark 非法: {mark}（只允许字母、数字、下划线和连字符，且不能包含首尾空白）"
        )));
    }
    Ok(())
}

/// `data/{engine}UserData[_mark]`，并断言落在 data/ 内。
pub fn resolve_user_data_dir(
    data_dir: &Path,
    mark: Option<&str>,
    engine: &str,
) -> Result<PathBuf, ConfigError> {
    validate_user_data_mark(mark)?;
    let prefix = if engine == "clearcote" {
        "clearcoteUserData"
    } else {
        "camoufoxUserData"
    };
    let name = match mark.filter(|s| !s.is_empty()) {
        Some(m) => format!("{prefix}_{m}"),
        None => prefix.to_string(),
    };
    let dir = data_dir.join(&name);
    let data_canon_base = data_dir;
    if !dir.starts_with(data_canon_base) || dir.parent() != Some(data_dir) {
        return Err(ConfigError(format!("userData 目录越界: {}", dir.display())));
    }
    Ok(dir)
}

/// 代理三态合并：instance 显式禁用 > instance 启用 > 全局启用 > 直连。
pub fn resolve_proxy<'a>(
    global: Option<&'a Value>,
    instance: Option<&'a Value>,
) -> Option<&'a Value> {
    if let Some(p) = instance {
        if p.get("enable").and_then(Value::as_bool) == Some(false) {
            return None;
        }
        if p.get("enable").and_then(Value::as_bool) == Some(true) {
            return Some(p);
        }
    }
    if let Some(p) = global {
        if p.get("enable").and_then(Value::as_bool) == Some(true) {
            return Some(p);
        }
    }
    None
}

fn sanitize_clearcote_args(args: &[Value]) -> Result<Vec<Value>, ConfigError> {
    for a in args {
        let s = a.as_str().unwrap_or("");
        let token = s.split('=').next().unwrap_or("").trim();
        if FORBIDDEN_ARG_PREFIXES
            .iter()
            .any(|p| token == *p || token.starts_with(&format!("{p}=")))
        {
            return Err(ConfigError(format!(
                "clearcote.args 不允许覆盖安全敏感参数: {token}（请使用专用配置项）"
            )));
        }
        if token == "--no-sandbox" {
            return Err(ConfigError(
                "clearcote.args 不允许手写 --no-sandbox（请使用 browser.clearcote.sandbox: false）"
                    .to_string(),
            ));
        }
    }
    Ok(args.to_vec())
}

fn set_default(obj: &mut Map<String, Value>, key: &str, value: Value) {
    if !obj.contains_key(key) || obj.get(key).is_some_and(Value::is_null) {
        obj.insert(key.to_string(), value);
    }
}

fn obj_mut<'a>(root: &'a mut Value, key: &str) -> &'a mut Map<String, Value> {
    if !root.get(key).is_some_and(Value::is_object) {
        root[key] = json!({});
    }
    root[key].as_object_mut().unwrap()
}

/// 加载、补默认值、校验并扁平化。返回的 JSON 结构与 JS 版 loadConfig() 一致。
pub fn load_config(root: &Path) -> Result<Value, ConfigError> {
    load_config_in(root, &root.join("data"))
}

pub fn load_config_in(root: &Path, data_dir: &Path) -> Result<Value, ConfigError> {
    let path = resolve_config_path_in(root, data_dir)?;
    let text = fs::read_to_string(&path).map_err(|e| ConfigError(format!("读取配置失败: {e}")))?;
    let yaml_value: serde_yaml::Value = serde_yaml::from_str(&text)
        .map_err(|e| ConfigError(format!("配置文件解析失败: {path:?}: {e}")))?;
    let mut config: Value =
        serde_json::to_value(&yaml_value).map_err(|e| ConfigError(format!("配置转换失败: {e}")))?;
    if !config.is_object() {
        return Err(ConfigError(format!("配置文件解析失败: {}", path.display())));
    }

    // Docker 路径兼容
    let browser_path = config
        .pointer("/browser/path")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if (browser_path.is_empty() || !Path::new(&browser_path).exists())
        && Path::new("/app/camoufox/camoufox").exists()
    {
        crate::logfmt::info(
            "配置器",
            "检测到容器环境，自动修正浏览器路径为 /app/camoufox/camoufox",
        );
        obj_mut(&mut config, "browser").insert("path".to_string(), json!("/app/camoufox/camoufox"));
    }

    // server.port 必填且为 1-65535 整数
    let port = config.pointer("/server/port").and_then(Value::as_i64);
    match port {
        Some(p) if (1..=65535).contains(&p) => {}
        Some(p) => {
            return Err(ConfigError(format!(
                "server.port 必须是 1-65535 范围内的整数，当前值: {p}"
            )))
        }
        None => return Err(ConfigError("配置文件缺少必需字段: server.port".to_string())),
    }
    crate::typed::Config::try_from_value(&config)
        .map_err(|e| ConfigError(format!("配置字段类型无效: {e}")))?;
    match config.pointer("/server/auth").and_then(Value::as_str) {
        None | Some("") => {
            crate::logfmt::warn(
                "配置器",
                "server.auth 未配置！API 和 WebUI 将无需认证即可访问！",
            );
            crate::logfmt::warn(
                "配置器",
                "请勿在公网环境中留空 auth，建议使用 webai2api-genkey 生成密钥",
            );
        }
        Some("sk-change-me-to-your-secure-key") => {
            crate::logfmt::warn("配置器", "检测到默认密钥！请勿在公网环境中使用默认密钥");
        }
        Some(a) if a.chars().count() < 10 => {
            crate::logfmt::warn(
                "配置器",
                "server.auth 长度少于 10 个字符，安全性较低，建议使用 webai2api-genkey 生成密钥",
            );
        }
        _ => {}
    }

    // keepalive.mode：非法值警告回退
    {
        let server = obj_mut(&mut config, "server");
        if !server.get("keepalive").is_some_and(Value::is_object) {
            server.insert("keepalive".to_string(), json!({ "mode": "comment" }));
        }
        let keepalive = server
            .get_mut("keepalive")
            .unwrap()
            .as_object_mut()
            .unwrap();
        set_default(keepalive, "mode", json!("comment"));
        let mode = keepalive
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if mode != "comment" && mode != "content" {
            crate::logfmt::warn(
                "配置器",
                &format!("无效的 keepalive.mode: {mode}，使用默认值 comment"),
            );
            keepalive.insert("mode".to_string(), json!("comment"));
        }
    }

    // browser 默认值
    let engine = {
        let browser = obj_mut(&mut config, "browser");
        set_default(browser, "humanizeCursor", json!("camou"));
        let engine = normalize_engine(browser.get("engine").and_then(Value::as_str))?;
        browser.insert("engine".to_string(), json!(engine));

        if !browser.get("clearcote").is_some_and(Value::is_object) {
            browser.insert("clearcote".to_string(), json!({}));
        }
        let cc = browser
            .get_mut("clearcote")
            .unwrap()
            .as_object_mut()
            .unwrap();
        set_default(cc, "path", json!(""));
        set_default(cc, "platform", json!("auto"));
        set_default(cc, "brand", json!("Chrome"));
        set_default(cc, "fingerprintProfile", json!(""));
        set_default(cc, "timezone", json!(""));
        set_default(cc, "acceptLanguage", json!(""));
        set_default(cc, "geoip", json!(true));
        set_default(cc, "humanize", json!(true));
        set_default(cc, "webrtcIp", json!(""));
        set_default(cc, "sandbox", json!(true));
        set_default(cc, "allowDetectedLicense", json!(false));
        let args = cc.get("args").cloned().unwrap_or(json!([]));
        let args = args.as_array().cloned().unwrap_or_default();
        let args = sanitize_clearcote_args(&args)?;
        let platform = cc
            .get("platform")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if !["auto", "windows", "linux"].contains(&platform.to_ascii_lowercase().as_str()) {
            return Err(ConfigError(format!(
                "browser.clearcote.platform 非法: {platform}（允许: auto | windows | linux）"
            )));
        }
        cc.insert("args".to_string(), json!(args));

        if !browser.get("camoufox").is_some_and(Value::is_object) {
            browser.insert("camoufox".to_string(), json!({}));
        }
        let camou = browser
            .get_mut("camoufox")
            .unwrap()
            .as_object_mut()
            .unwrap();
        set_default(camou, "mainWorldEval", json!(false));
        set_default(camou, "enableCache", json!(false));
        set_default(camou, "disableInstantAnimations", json!(false));
        set_default(camou, "humanizeMaxTime", json!(1.5));
        set_default(camou, "blockWebRtc", json!(true));
        set_default(camou, "geoip", json!(true));
        set_default(camou, "locale", Value::Null);
        for key in ["certificates", "certificatePaths"] {
            if !camou.get(key).is_some_and(Value::is_array) {
                camou.insert(key.to_string(), json!([]));
            }
        }
        engine
    };

    // pool 默认值
    {
        let backend = obj_mut(&mut config, "backend");
        if !backend.get("pool").is_some_and(Value::is_object) {
            backend.insert("pool".to_string(), json!({}));
        }
        let pool = backend.get_mut("pool").unwrap().as_object_mut().unwrap();
        set_default(pool, "strategy", json!("least_busy"));
        let strategy = pool
            .get("strategy")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if !["least_busy", "round_robin", "random"].contains(&strategy.as_str()) {
            crate::logfmt::warn(
                "配置器",
                &format!("无效的 pool.strategy: {strategy}，使用默认值 least_busy"),
            );
            pool.insert("strategy".to_string(), json!("least_busy"));
        }
        if !pool.get("failover").is_some_and(Value::is_object) {
            pool.insert("failover".to_string(), json!({}));
        }
        let failover = pool.get_mut("failover").unwrap().as_object_mut().unwrap();
        set_default(failover, "enabled", json!(true));
        set_default(failover, "maxRetries", json!(2));
        set_default(failover, "imgDlRetry", json!(false));
        set_default(failover, "imgDlRetryMaxRetries", json!(2));
    }

    // instances 校验 + 扁平化
    let instances = config
        .pointer("/backend/pool/instances")
        .and_then(Value::as_array)
        .cloned();
    let instances = match instances {
        Some(v) if !v.is_empty() => v,
        Some(_) => {
            return Err(ConfigError(
                "backend.pool.instances 不能为空数组".to_string(),
            ))
        }
        None => {
            return Err(ConfigError(
                "配置文件缺少必需字段: backend.pool.instances".to_string(),
            ))
        }
    };
    let global_proxy = config.pointer("/browser/proxy").cloned();
    let workers = flatten_instances(&instances, global_proxy.as_ref(), &engine, data_dir)?;

    let has_gemini_biz = workers.iter().any(|w| {
        w["type"] == "gemini_biz"
            || (w["type"] == "merge"
                && w["mergeTypes"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|t| t == "gemini_biz")))
    });
    if has_gemini_biz
        && config
            .pointer("/backend/adapter/gemini_biz/entryUrl")
            .and_then(Value::as_str)
            .unwrap_or("")
            .is_empty()
    {
        return Err(ConfigError(
            "存在 gemini_biz 类型的 Worker，但 backend.adapter.gemini_biz.entryUrl 未配置"
                .to_string(),
        ));
    }

    let worker_count = workers.len();
    let engines = referenced_engines(&config, &workers);
    {
        let pool = config["backend"]["pool"].as_object_mut().unwrap();
        pool.insert("workers".to_string(), json!(workers));
        pool.insert("referencedEngines".to_string(), json!(engines));
    }

    // queue 默认值；maxConcurrent = worker 数
    {
        let root_obj = config.as_object_mut().unwrap();
        if !root_obj.get("queue").is_some_and(Value::is_object) {
            root_obj.insert("queue".to_string(), json!({}));
        }
        let queue = root_obj.get_mut("queue").unwrap().as_object_mut().unwrap();
        set_default(queue, "queueBuffer", json!(2));
        set_default(queue, "imageLimit", json!(5));
        queue.insert("maxConcurrent".to_string(), json!(worker_count));
    }
    {
        let backend = config["backend"].as_object_mut().unwrap();
        if !backend.get("adapter").is_some_and(Value::is_object) {
            backend.insert("adapter".to_string(), json!({}));
        }
    }

    if let Some(level) = config.get("logLevel").and_then(Value::as_str) {
        if ["debug", "info", "warn", "error"].contains(&level) {
            crate::logfmt::init(&data_dir.join("logs"), level);
        }
    }

    crate::logfmt::debug("配置器", &format!("已加载配置文件: {}", path.display()));
    crate::logfmt::debug(
        "配置器",
        &format!("Instances: {}, Workers: {worker_count}", instances.len()),
    );
    Ok(config)
}

fn referenced_engines(config: &Value, workers: &[Value]) -> Vec<String> {
    let mut set = vec![config
        .pointer("/browser/engine")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_ENGINE)
        .to_string()];
    for w in workers {
        if let Some(e) = w.get("engine").and_then(Value::as_str) {
            if !set.contains(&e.to_string()) {
                set.push(e.to_string());
            }
        }
    }
    set
}

fn flatten_instances(
    instances: &[Value],
    global_proxy: Option<&Value>,
    default_engine: &str,
    data_dir: &Path,
) -> Result<Vec<Value>, ConfigError> {
    let mut workers = Vec::new();
    let mut names = Vec::new();
    for (i, instance) in instances.iter().enumerate() {
        let iname = instance.get("name").and_then(Value::as_str).unwrap_or("");
        if iname.is_empty() {
            return Err(ConfigError(format!("instances[{i}] 缺少必需字段: name")));
        }
        let iworkers = instance.get("workers").and_then(Value::as_array);
        if iworkers.is_none() || iworkers.is_some_and(|w| w.is_empty()) {
            return Err(ConfigError(format!(
                "instances[{i}] ({iname}) 缺少有效的 workers 数组"
            )));
        }
        let engine = match instance.get("engine").and_then(Value::as_str) {
            Some(e) if !e.is_empty() => normalize_engine(Some(e)).map_err(|err| {
                ConfigError(format!("backend.pool.instances[{i}] ({iname}): {}", err.0))
            })?,
            _ => default_engine.to_string(),
        };
        let mark = instance.get("userDataMark").and_then(Value::as_str);
        let user_data_dir = resolve_user_data_dir(data_dir, mark, &engine)?;
        let proxy = resolve_proxy(global_proxy, instance.get("proxy")).cloned();

        for (j, worker) in iworkers.unwrap().iter().enumerate() {
            let wname = worker.get("name").and_then(Value::as_str).unwrap_or("");
            if wname.is_empty() {
                return Err(ConfigError(format!(
                    "instances[{iname}].workers[{j}] 缺少必需字段: name"
                )));
            }
            let wtype = worker.get("type").and_then(Value::as_str).unwrap_or("");
            if wtype.is_empty() {
                return Err(ConfigError(format!(
                    "instances[{iname}].workers[{j}] ({wname}) 缺少必需字段: type"
                )));
            }
            if wtype == "merge"
                && worker
                    .get("mergeTypes")
                    .and_then(Value::as_array)
                    .is_none_or(|a| a.is_empty())
            {
                return Err(ConfigError(format!(
                    "Worker \"{wname}\" 类型为 merge，但缺少有效的 mergeTypes 数组"
                )));
            }
            if names.contains(&wname.to_string()) {
                return Err(ConfigError(format!(
                    "Worker 名称 \"{wname}\" 重复。Worker 名称必须全局唯一。"
                )));
            }
            names.push(wname.to_string());
            workers.push(json!({
                "name": wname,
                "type": wtype,
                "mergeTypes": worker.get("mergeTypes").cloned().unwrap_or(json!([])),
                "mergeMonitor": worker.get("mergeMonitor").cloned().unwrap_or(Value::Null),
                "instanceName": iname,
                "engine": engine,
                "userDataMark": mark,
                "userDataDir": user_data_dir.to_string_lossy(),
                "resolvedProxy": proxy,
            }));
        }
    }
    Ok(workers)
}

/// 配置访问便捷方法：底层走 typed 视图（serde 结构体），默认值与回退语义集中一处。
pub fn port_of(config: &Value) -> u16 {
    crate::typed::Config::from_value(config).port()
}
pub fn auth_of(config: &Value) -> String {
    crate::typed::Config::from_value(config).auth()
}
pub fn keepalive_mode(config: &Value) -> String {
    crate::typed::Config::from_value(config).keepalive_mode()
}
pub fn queue_buffer(config: &Value) -> u64 {
    crate::typed::Config::from_value(config).queue_buffer()
}
pub fn image_limit(config: &Value) -> u64 {
    crate::typed::Config::from_value(config).image_limit()
}
pub fn max_concurrent(config: &Value) -> u64 {
    crate::typed::Config::from_value(config).max_concurrent()
}
pub fn image_markdown(config: &Value) -> bool {
    crate::typed::Config::from_value(config).image_markdown()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_cfg(dir: &Path, yaml: &str) {
        fs::create_dir_all(dir.join("data")).unwrap();
        fs::write(dir.join("data/config.yaml"), yaml).unwrap();
    }

    #[test]
    fn defaults_and_flatten() {
        let dir = std::env::temp_dir().join(format!("webai2api-cfg-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        write_cfg(
            &dir,
            r#"
server: { port: 3000, auth: "sk-test-key-123" }
backend:
  pool:
    instances:
      - name: main
        workers:
          - { name: w1, type: gemini }
          - { name: w2, type: merge, mergeTypes: [gemini] }
"#,
        );
        let cfg = load_config(&dir).unwrap();
        assert_eq!(cfg.pointer("/server/keepalive/mode").unwrap(), "comment");
        assert_eq!(cfg.pointer("/browser/engine").unwrap(), "camoufox");
        assert_eq!(cfg.pointer("/browser/humanizeCursor").unwrap(), "camou");
        assert_eq!(
            cfg.pointer("/browser/clearcote/sandbox").unwrap(),
            &json!(true)
        );
        assert_eq!(cfg.pointer("/backend/pool/strategy").unwrap(), "least_busy");
        assert_eq!(cfg.pointer("/queue/maxConcurrent").unwrap(), &json!(2));
        assert_eq!(cfg.pointer("/queue/queueBuffer").unwrap(), &json!(2));
        let workers = cfg
            .pointer("/backend/pool/workers")
            .unwrap()
            .as_array()
            .unwrap();
        assert!(workers[0]["userDataDir"]
            .as_str()
            .unwrap()
            .ends_with("camoufoxUserData"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_values() {
        let dir = std::env::temp_dir().join(format!("webai2api-cfg2-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        write_cfg(
            &dir,
            "server: { port: 70000, auth: '' }\nbackend: { pool: { instances: [] } }\n",
        );
        let err = load_config(&dir).unwrap_err();
        assert!(err.0.contains("1-65535"), "{}", err.0);

        write_cfg(
            &dir,
            r#"
server: { port: 3000, auth: "sk-test-key-123" }
backend:
  pool:
    instances:
      - { name: main, engine: chromium, workers: [ { name: w, type: t } ] }
"#,
        );
        let err = load_config(&dir).unwrap_err();
        assert!(err.0.contains("未知 browser engine"), "{}", err.0);

        write_cfg(
            &dir,
            r#"
server: { port: 3000, auth: "sk-test-key-123" }
browser:
  clearcote: { args: ["--no-sandbox"] }
backend:
  pool:
    instances:
      - { name: main, workers: [ { name: w, type: t } ] }
"#,
        );
        let err = load_config(&dir).unwrap_err();
        assert!(err.0.contains("--no-sandbox"), "{}", err.0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn proxy_merge_and_mark() {
        let g = json!({"enable": true, "host": "global"});
        let disabled = json!({"enable": false});
        assert!(resolve_proxy(Some(&g), Some(&disabled)).is_none());
        let inst = json!({"enable": true, "host": "inst"});
        assert_eq!(
            resolve_proxy(Some(&g), Some(&inst)).unwrap()["host"],
            "inst"
        );
        assert_eq!(resolve_proxy(Some(&g), None).unwrap()["host"], "global");

        let data = Path::new("/tmp/data");
        assert!(resolve_user_data_dir(data, Some("../x"), "camoufox").is_err());
        assert!(resolve_user_data_dir(data, Some(" ok"), "camoufox").is_err());
        let d = resolve_user_data_dir(data, Some("a_1"), "clearcote").unwrap();
        assert!(d.ends_with("clearcoteUserData_a_1"));
    }

    #[test]
    fn fallback_enums_warn_not_throw() {
        let dir = std::env::temp_dir().join(format!("webai2api-cfg3-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        write_cfg(
            &dir,
            r#"
server: { port: 3000, auth: "sk-test-key-123", keepalive: { mode: nonsense } }
backend:
  pool:
    strategy: bogus
    instances:
      - { name: main, workers: [ { name: w, type: t } ] }
"#,
        );
        let cfg = load_config(&dir).unwrap();
        assert_eq!(cfg.pointer("/server/keepalive/mode").unwrap(), "comment");
        assert_eq!(cfg.pointer("/backend/pool/strategy").unwrap(), "least_busy");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn custom_data_dir_and_typed_auth_errors_are_explicit() {
        let root = std::env::temp_dir().join(format!("webai2api-root-{}", std::process::id()));
        let data = std::env::temp_dir().join(format!("webai2api-data-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&data);
        fs::create_dir_all(&data).unwrap();
        fs::write(
            data.join("config.yaml"),
            "server: { port: 3000, auth: 'sk-custom-auth-123' }\nbackend: { pool: { instances: [{ name: main, workers: [{ name: w, type: mock }] }] } }\n",
        )
        .unwrap();
        let cfg = load_config_in(&root, &data).unwrap();
        assert_eq!(cfg["server"]["auth"], "sk-custom-auth-123");

        fs::write(
            data.join("config.yaml"),
            "server: { port: 3000, auth: 1234567890 }\nbackend: { pool: { instances: [{ name: main, workers: [{ name: w, type: mock }] }] } }\n",
        )
        .unwrap();
        let error = load_config_in(&root, &data).unwrap_err();
        assert!(error.0.contains("配置字段类型无效"), "{}", error.0);
        assert!(error.0.contains("string"), "{}", error.0);
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&data);
    }
}
