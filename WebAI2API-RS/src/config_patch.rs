//! 配置写回（WebUI POST /admin/config/* → config.yaml）。
//! 校验器逐条对齐原 `src/config/validator.js` 的文案与判定；
//! WebUI 用 authToken / keepaliveMode / logLevel；YAML 用 server.auth / keepalive.mode / 根 logLevel。

use serde_json::Value;
use std::path::Path;

fn yaml_set(target: &mut serde_yaml::Value, key: &str, value: serde_yaml::Value) {
    if !target.is_mapping() {
        *target = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    }
    target
        .as_mapping_mut()
        .unwrap()
        .insert(serde_yaml::Value::String(key.to_string()), value);
}

fn to_yaml(v: &Value) -> serde_yaml::Value {
    serde_json::from_value(v.clone()).unwrap_or(serde_yaml::Value::Null)
}

/// 扫描 `src-root/src/backend/adapter/*.js` 得到适配器 ID（Node registry 的键即 manifest.id，
/// 实测与文件名一致）。目录缺失时返回空表，让校验按"未知类型"拒绝而非 panic。
pub fn adapter_ids_from_src(src_root: &Path) -> Vec<String> {
    let dir = src_root.join("src/backend/adapter");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.strip_suffix(".js").map(str::to_string)
        })
        .collect();
    ids.sort();
    ids
}

/// 校验结果：错误信息列表（为空即通过）。文案与原版逐条一致。
pub fn validate_server_patch(d: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    if let Some(v) = d.get("port") {
        let is_int = v.as_i64().is_some() || v.as_u64().is_some();
        if !is_int {
            errors.push("port 必须是整数".into());
        } else if let Some(n) = v.as_i64() {
            if !(1..=65535).contains(&n) {
                errors.push("port 必须在 1-65535 范围内".into());
            }
        }
    }
    if let Some(v) = d.get("authToken") {
        if !v.is_string() {
            errors.push("authToken 必须是字符串".into());
        } else {
            let s = v.as_str().unwrap_or_default();
            if !s.is_empty() && s.chars().count() < 10 {
                errors.push("authToken 如果设置则必须至少 10 个字符，或留空".into());
            }
        }
    }
    if let Some(v) = d.get("keepaliveMode") {
        if !matches!(v.as_str(), Some("comment") | Some("content")) {
            errors.push("keepaliveMode 必须是 comment 或 content".into());
        }
    }
    if let Some(v) = d.get("logLevel") {
        if !matches!(
            v.as_str(),
            Some("debug") | Some("info") | Some("warn") | Some("error")
        ) {
            errors.push("logLevel 必须是 debug、info、warn 或 error".into());
        }
    }
    if let Some(v) = d.get("queueBuffer") {
        let is_int = v.as_i64().is_some() || v.as_u64().is_some();
        if !is_int {
            errors.push("queueBuffer 必须是整数".into());
        } else if v.as_i64().is_some_and(|n| n < 0) {
            errors.push("queueBuffer 不能为负数".into());
        }
    }
    if let Some(v) = d.get("imageLimit") {
        let is_int = v.as_i64().is_some() || v.as_u64().is_some();
        if !is_int {
            errors.push("imageLimit 必须是整数".into());
        } else if let Some(n) = v.as_u64() {
            if !(1..=10).contains(&n) {
                errors.push("imageLimit 必须在 1-10 范围内".into());
            }
        }
    }
    if let Some(v) = d.get("imageMarkdown") {
        if !v.is_boolean() {
            errors.push("imageMarkdown 必须是布尔值".into());
        }
    }
    errors
}

pub fn validate_browser_patch(d: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    if let Some(v) = d.get("path") {
        if !v.is_string() {
            errors.push("path 必须是字符串".into());
        }
    }
    if let Some(v) = d.get("engine") {
        if !v.is_null() && v.as_str() != Some("") {
            let ok = v.as_str().is_some_and(|s| {
                matches!(s.to_ascii_lowercase().as_str(), "camoufox" | "clearcote")
            });
            if !ok {
                errors.push("engine 必须是 camoufox 或 clearcote".into());
            }
        }
    }
    if let Some(cc) = d.get("clearcote") {
        if cc.is_object() {
            if let Some(p) = cc.get("path") {
                if !p.is_string() {
                    errors.push("clearcote.path 必须是字符串".into());
                }
            }
            if let Some(p) = cc.get("platform") {
                let ok = p.as_str().is_some_and(|s| {
                    matches!(
                        s.to_ascii_lowercase().as_str(),
                        "auto" | "windows" | "linux"
                    )
                });
                if !ok {
                    errors.push("clearcote.platform 必须是 auto、windows 或 linux".into());
                }
            }
            if let Some(b) = cc.get("brand") {
                if !matches!(
                    b.as_str(),
                    Some("Chrome") | Some("Edge") | Some("Opera") | Some("Vivaldi")
                ) {
                    errors.push("clearcote.brand 必须是 Chrome、Edge、Opera 或 Vivaldi".into());
                }
            }
            for (key, msg) in [
                ("geoip", "clearcote.geoip 必须是布尔值"),
                ("humanize", "clearcote.humanize 必须是布尔值"),
                ("sandbox", "clearcote.sandbox 必须是布尔值"),
                (
                    "allowDetectedLicense",
                    "clearcote.allowDetectedLicense 必须是布尔值",
                ),
            ] {
                if let Some(v) = cc.get(key) {
                    if !v.is_boolean() {
                        errors.push(msg.into());
                    }
                }
            }
            for (key, msg) in [
                (
                    "fingerprintProfile",
                    "clearcote.fingerprintProfile 必须是字符串路径",
                ),
                ("timezone", "clearcote.timezone 必须是字符串"),
                ("acceptLanguage", "clearcote.acceptLanguage 必须是字符串"),
                ("webrtcIp", "clearcote.webrtcIp 必须是字符串"),
            ] {
                if let Some(v) = cc.get(key) {
                    if !v.is_string() {
                        errors.push(msg.into());
                    }
                }
            }
            if let Some(args) = cc.get("args") {
                match args.as_array() {
                    None => errors.push("clearcote.args 必须是字符串数组".into()),
                    Some(list) => {
                        // 非字符串元素与敏感参数的具体文案由 sanitize 逐条给出
                        if let Err(e) = sanitize_clearcote_args(list) {
                            errors.push(e);
                        }
                    }
                }
            }
            for forbidden in [
                "ffVersion",
                "camoufox",
                "firefox_user_prefs",
                "webgl_config",
            ] {
                if cc.get(forbidden).is_some() {
                    errors.push(format!(
                        "clearcote 不允许 Firefox/Camoufox 专属字段: {forbidden}"
                    ));
                }
            }
        } else if !cc.is_null() {
            errors.push("clearcote 必须是对象".into());
        }
    }
    for (key, msg) in [
        ("headless", "headless 必须是布尔值"),
        ("fission", "fission 必须是布尔值"),
    ] {
        if let Some(v) = d.get(key) {
            if !v.is_boolean() {
                errors.push(msg.into());
            }
        }
    }
    if let Some(proxy) = d.get("proxy") {
        if let Some(v) = proxy.get("enable") {
            if !v.is_boolean() {
                errors.push("proxy.enable 必须是布尔值".into());
            }
        }
        if let Some(v) = proxy.get("type") {
            if !matches!(v.as_str(), Some("http") | Some("socks5")) {
                errors.push("proxy.type 必须是 http 或 socks5".into());
            }
        }
        if let Some(v) = proxy.get("port") {
            let is_int = v.as_i64().is_some() || v.as_u64().is_some();
            if !is_int {
                errors.push("proxy.port 必须是整数".into());
            } else if let Some(n) = v.as_i64() {
                if !(1..=65535).contains(&n) {
                    errors.push("proxy.port 必须在 1-65535 范围内".into());
                }
            }
        }
    }
    errors
}

/// clearcote.args 校验（对应 engineContract.js sanitizeClearcoteArgs）：逐条匹配原版文案。
fn sanitize_clearcote_args(args: &[Value]) -> Result<(), String> {
    const SENSITIVE: [&str; 8] = [
        "--user-data-dir",
        "--proxy-server",
        "--proxy-bypass-list",
        "--fingerprint",
        "--fingerprint-profile",
        "--remote-debugging-port",
        "--remote-debugging-pipe",
        "--headless",
    ];
    for a in args {
        if !a.is_string() {
            let kind = match a {
                Value::Null => "object",
                Value::Bool(_) => "boolean",
                Value::Number(_) => "number",
                Value::String(_) => "string",
                Value::Array(_) => "object",
                Value::Object(_) => "object",
            };
            return Err(format!("clearcote.args 必须全是字符串，发现: {kind}"));
        }
        let trimmed = a.as_str().unwrap_or_default().trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        for bad in SENSITIVE {
            if lower == bad
                || lower.starts_with(&format!("{bad}="))
                || lower.starts_with(&format!("{bad} "))
            {
                return Err(format!(
                    "clearcote.args 不允许覆盖安全敏感参数: {bad}（请使用专用配置项）"
                ));
            }
        }
        // --no-sandbox 只能通过 sandbox: false 表达，避免 UI/配置双路径
        if lower == "--no-sandbox" || lower.starts_with("--no-sandbox=") {
            return Err(
                "clearcote.args 不允许直接写 --no-sandbox，请设置 browser.clearcote.sandbox: false"
                    .into(),
            );
        }
    }
    Ok(())
}

pub fn validate_instances_patch(list: &[Value], adapter_ids: &[String]) -> Vec<String> {
    let mut errors = Vec::new();
    if list.is_empty() {
        errors.push("instances 不能为空".into());
        return errors;
    }
    let mut instance_names = std::collections::HashSet::new();
    let mut worker_names = std::collections::HashSet::new();
    let valid_type = |t: &str| t == "merge" || adapter_ids.iter().any(|a| a == t);
    for (i, inst) in list.iter().enumerate() {
        let prefix = format!("instances[{i}]");
        match inst.get("name").and_then(Value::as_str) {
            None => errors.push(format!("{prefix}: name 是必填字段且必须是字符串")),
            Some(n) if n.trim().is_empty() => errors.push(format!("{prefix}: name 不能为空")),
            Some(n) if instance_names.contains(n) => {
                errors.push(format!("{prefix}: Instance 名称 \"{n}\" 重复"))
            }
            Some(n) => {
                instance_names.insert(n.to_string());
            }
        }
        if let Some(engine) = inst.get("engine") {
            if !engine.is_null() && engine.as_str() != Some("") {
                let ok = engine.as_str().is_some_and(|s| {
                    matches!(s.to_ascii_lowercase().as_str(), "camoufox" | "clearcote")
                });
                if !ok {
                    errors.push(format!("{prefix}: engine 必须是 camoufox 或 clearcote（或省略以继承 browser.engine）"));
                }
            }
        }
        if let Some(mark) = inst.get("userDataMark") {
            if !mark.is_null() && mark.as_str() != Some("") {
                let m = mark.as_str().unwrap_or_default();
                if m.trim() != m
                    || !m
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                {
                    errors.push(format!("{prefix}: userDataMark 非法: {m}（只允许字母、数字、下划线和连字符，且不能包含首尾空白）"));
                }
            }
        }
        if let Some(proxy) = inst.get("proxy") {
            if let Some(v) = proxy.get("enable") {
                if !v.is_boolean() {
                    errors.push(format!("{prefix}.proxy: enable 必须是布尔值"));
                }
            }
            if let Some(v) = proxy.get("type") {
                if !matches!(v.as_str(), Some("http") | Some("socks5")) {
                    errors.push(format!("{prefix}.proxy: type 必须是 http 或 socks5"));
                }
            }
            if let Some(v) = proxy.get("port") {
                match v.as_u64() {
                    None => errors.push(format!("{prefix}.proxy: port 必须是数字")),
                    Some(n) if !(1..=65535).contains(&n) => {
                        errors.push(format!("{prefix}.proxy: port 必须在 1-65535 范围内"))
                    }
                    _ => {}
                }
            }
        }
        match inst.get("workers").and_then(Value::as_array) {
            None => errors.push(format!("{prefix}: workers 是必填字段且必须是数组")),
            Some(ws) if ws.is_empty() => errors.push(format!("{prefix}: workers 不能为空")),
            Some(ws) => {
                for (j, w) in ws.iter().enumerate() {
                    let w_prefix = format!("{prefix}.workers[{j}]");
                    match w.get("name").and_then(Value::as_str) {
                        None => errors.push(format!("{w_prefix}: name 是必填字段")),
                        Some(n) if n.trim().is_empty() => {
                            errors.push(format!("{w_prefix}: name 不能为空"))
                        }
                        Some(n) if worker_names.contains(n) => {
                            errors.push(format!("{w_prefix}: Worker 名称 \"{n}\" 全局重复（Worker 名称必须全局唯一）"));
                        }
                        Some(n) => {
                            worker_names.insert(n.to_string());
                        }
                    }
                    match w.get("type").and_then(Value::as_str) {
                        None => errors.push(format!("{w_prefix}: type 是必填字段")),
                        Some(t) if !valid_type(t) => {
                            errors.push(format!("{w_prefix}: type \"{t}\" 不是有效的适配器类型"))
                        }
                        Some(_) => {}
                    }
                    if w.get("type").and_then(Value::as_str) == Some("merge") {
                        match w.get("mergeTypes").and_then(Value::as_array) {
                            None => errors
                                .push(format!("{w_prefix}: merge 类型必须指定 mergeTypes 数组")),
                            Some(mts) if mts.is_empty() => errors
                                .push(format!("{w_prefix}: merge 类型必须指定 mergeTypes 数组")),
                            Some(mts) => {
                                for mt in mts {
                                    let mt = mt.as_str().unwrap_or_default();
                                    if !valid_type(mt) || mt == "merge" {
                                        errors.push(format!("{w_prefix}: mergeTypes 中的 \"{mt}\" 不是有效的适配器类型"));
                                    }
                                }
                                if let Some(monitor) = w.get("mergeMonitor").and_then(Value::as_str)
                                {
                                    let in_list = mts.iter().any(|m| m.as_str() == Some(monitor));
                                    if !in_list {
                                        errors.push(format!("{w_prefix}: mergeMonitor \"{monitor}\" 必须是 mergeTypes 中的一个"));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    errors
}

pub fn validate_pool_patch(d: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    if let Some(v) = d.get("strategy") {
        if !matches!(
            v.as_str(),
            Some("least_busy") | Some("round_robin") | Some("random")
        ) {
            errors.push("strategy 必须是 least_busy、round_robin 或 random".into());
        }
    }
    if let Some(f) = d.get("failover") {
        if let Some(v) = f.get("enabled") {
            if !v.is_boolean() {
                errors.push("failover.enabled 必须是布尔值".into());
            }
        }
        if let Some(v) = f.get("maxRetries") {
            // JSON 数字先按"是否整数"判定，负数也是整数（原版 Number.isInteger(-1) === true）
            let is_int = v.as_i64().is_some() || v.as_u64().is_some();
            if !is_int {
                errors.push("failover.maxRetries 必须是整数".into());
            } else if v.as_i64().is_some_and(|n| n < 0) {
                errors.push("failover.maxRetries 不能为负数".into());
            }
        }
        if let Some(v) = f.get("imgDlRetry") {
            if !v.is_boolean() {
                errors.push("failover.imgDlRetry 必须是布尔值".into());
            }
        }
        if let Some(v) = f.get("imgDlRetryMaxRetries") {
            let is_int = v.as_i64().is_some() || v.as_u64().is_some();
            if !is_int {
                errors.push("failover.imgDlRetryMaxRetries 必须是整数".into());
            } else if let Some(n) = v.as_u64() {
                if !(1..=10).contains(&n) {
                    errors.push("failover.imgDlRetryMaxRetries 必须在 1-10 范围内".into());
                }
            }
        }
    }
    errors
}

pub fn validate_adapters_patch(d: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    if !d.is_object() {
        errors.push("adapters 配置必须是对象".into());
        return errors;
    }
    if let Some(g) = d.get("gemini_biz") {
        if let Some(u) = g.get("entryUrl") {
            match u.as_str() {
                None => errors.push("gemini_biz.entryUrl 必须是字符串".into()),
                Some(s) if !s.is_empty() && !s.starts_with("https://") => {
                    errors.push("gemini_biz.entryUrl 必须以 https:// 开头".into());
                }
                Some(_) => {}
            }
        }
    }
    errors
}

pub fn apply_server_patch(yaml: &mut serde_yaml::Value, patch: &Value) {
    if let Some(port) = patch.get("port") {
        yaml_set(&mut yaml["server"], "port", to_yaml(port));
    }
    if let Some(auth) = patch.get("authToken").or_else(|| patch.get("auth")) {
        yaml_set(&mut yaml["server"], "auth", to_yaml(auth));
    }
    if let Some(mode) = patch.get("keepaliveMode").and_then(Value::as_str) {
        yaml_set(
            &mut yaml["server"]["keepalive"],
            "mode",
            serde_yaml::Value::String(mode.to_string()),
        );
    }
    if let Some(level) = patch.get("logLevel").and_then(Value::as_str) {
        yaml_set(
            yaml,
            "logLevel",
            serde_yaml::Value::String(level.to_string()),
        );
    }
    if let Some(md) = patch.get("imageMarkdown") {
        yaml_set(&mut yaml["server"], "imageMarkdown", to_yaml(md));
    }
    if let Some(buf) = patch.get("queueBuffer") {
        yaml_set(&mut yaml["queue"], "queueBuffer", to_yaml(buf));
    }
    if let Some(limit) = patch.get("imageLimit") {
        yaml_set(&mut yaml["queue"], "imageLimit", to_yaml(limit));
    }
}

/// 浏览器页的 proxy.username/password 写回 YAML 的 user/passwd。
pub fn apply_browser_patch(yaml: &mut serde_yaml::Value, patch: &Value) {
    for key in [
        "path",
        "engine",
        "headless",
        "fission",
        "humanizeCursor",
        "ffVersion",
    ] {
        if let Some(v) = patch.get(key) {
            yaml_set(&mut yaml["browser"], key, to_yaml(v));
        }
    }
    if let Some(cam) = patch.get("camoufox").and_then(Value::as_object) {
        for (k, v) in cam {
            yaml_set(&mut yaml["browser"]["camoufox"], k, to_yaml(v));
        }
    }
    if let Some(cc) = patch.get("clearcote").and_then(Value::as_object) {
        for (k, v) in cc {
            yaml_set(&mut yaml["browser"]["clearcote"], k, to_yaml(v));
        }
    }
    if let Some(css) = patch.get("cssInject").and_then(Value::as_object) {
        for (k, v) in css {
            yaml_set(&mut yaml["browser"]["cssInject"], k, to_yaml(v));
        }
    }
    if let Some(proxy) = patch.get("proxy").and_then(Value::as_object) {
        for key in ["enable", "type", "host", "port"] {
            if let Some(v) = proxy.get(key) {
                yaml_set(&mut yaml["browser"]["proxy"], key, to_yaml(v));
            }
        }
        if let Some(user) = proxy.get("username").or_else(|| proxy.get("user")) {
            yaml_set(&mut yaml["browser"]["proxy"], "user", to_yaml(user));
        }
        if let Some(pass) = proxy.get("password").or_else(|| proxy.get("passwd")) {
            yaml_set(&mut yaml["browser"]["proxy"], "passwd", to_yaml(pass));
        }
    }
}

/// 前端 waitTimeout 是秒，YAML 是毫秒。
pub fn apply_pool_patch(yaml: &mut serde_yaml::Value, patch: &Value) {
    if let Some(strategy) = patch.get("strategy") {
        yaml_set(&mut yaml["backend"]["pool"], "strategy", to_yaml(strategy));
    }
    if let Some(seconds) = patch.get("waitTimeout").and_then(Value::as_u64) {
        if seconds > 0 {
            yaml_set(
                &mut yaml["backend"]["pool"],
                "waitTimeout",
                serde_yaml::Value::Number((seconds * 1000).into()),
            );
        }
    }
    if let Some(failover) = patch.get("failover").and_then(Value::as_object) {
        let current = yaml["backend"]["pool"]["failover"].clone();
        let mut merged = serde_yaml::from_value::<Value>(current)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        for (k, v) in failover {
            merged.insert(k.clone(), v.clone());
        }
        yaml_set(
            &mut yaml["backend"]["pool"],
            "failover",
            to_yaml(&Value::Object(merged)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    #[test]
    fn config_patches_use_yaml_field_names() {
        let mut yaml = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        apply_server_patch(
            &mut yaml,
            &json!({
                "port": 3200, "authToken": "", "keepaliveMode": "content",
                "logLevel": "debug", "queueBuffer": 4, "imageLimit": 3, "imageMarkdown": true
            }),
        );
        assert_eq!(yaml["server"]["auth"].as_str().unwrap(), "");
        assert_eq!(
            yaml["server"]["keepalive"]["mode"].as_str().unwrap(),
            "content"
        );
        assert_eq!(yaml["logLevel"].as_str().unwrap(), "debug");
        assert_eq!(yaml["queue"]["queueBuffer"].as_u64().unwrap(), 4);
        apply_browser_patch(
            &mut yaml,
            &json!({"proxy": {"enable": true, "username": "name", "password": "secret"}}),
        );
        assert_eq!(yaml["browser"]["proxy"]["user"].as_str().unwrap(), "name");
        assert_eq!(
            yaml["browser"]["proxy"]["passwd"].as_str().unwrap(),
            "secret"
        );
        apply_pool_patch(
            &mut yaml,
            &json!({"strategy": "random", "waitTimeout": 90, "failover": {"maxRetries": 1}}),
        );
        assert_eq!(
            yaml["backend"]["pool"]["waitTimeout"].as_u64().unwrap(),
            90_000
        );
        assert_eq!(
            yaml["backend"]["pool"]["failover"]["maxRetries"]
                .as_u64()
                .unwrap(),
            1
        );
    }

    /// 逐条对齐 src/config/validator.js 的文案与判定（负数也是合法整数）。
    #[test]
    fn config_validators_match_node_messages() {
        assert_eq!(validate_server_patch(&json!({})), Vec::<String>::new());
        assert_eq!(
            validate_server_patch(&json!({"keepaliveMode": "bogus"})),
            vec!["keepaliveMode 必须是 comment 或 content"]
        );
        assert_eq!(
            validate_server_patch(&json!({"logLevel": "nope"})),
            vec!["logLevel 必须是 debug、info、warn 或 error"]
        );
        assert_eq!(
            validate_server_patch(&json!({"port": 99999})),
            vec!["port 必须在 1-65535 范围内"]
        );
        assert_eq!(
            validate_server_patch(&json!({"port": 1.5})),
            vec!["port 必须是整数"]
        );
        assert_eq!(
            validate_server_patch(&json!({"authToken": "short"})),
            vec!["authToken 如果设置则必须至少 10 个字符，或留空"]
        );
        assert_eq!(
            validate_server_patch(&json!({"imageLimit": 99})),
            vec!["imageLimit 必须在 1-10 范围内"]
        );
        assert_eq!(
            validate_server_patch(&json!({"queueBuffer": -1})),
            vec!["queueBuffer 不能为负数"]
        );
        assert_eq!(
            validate_server_patch(&json!({"imageMarkdown": "yes"})),
            vec!["imageMarkdown 必须是布尔值"]
        );
        assert_eq!(
            validate_server_patch(&json!({"logLevel": "debug", "port": 3000})),
            Vec::<String>::new()
        );

        assert_eq!(
            validate_browser_patch(&json!({"engine": "badengine"})),
            vec!["engine 必须是 camoufox 或 clearcote"]
        );
        assert_eq!(
            validate_browser_patch(&json!({"headless": "yes"})),
            vec!["headless 必须是布尔值"]
        );
        assert_eq!(
            validate_browser_patch(&json!({"clearcote": {"platform": "bsd"}})),
            vec!["clearcote.platform 必须是 auto、windows 或 linux"]
        );
        assert_eq!(
            validate_browser_patch(&json!({"clearcote": {"args": ["--headless"]}})),
            vec!["clearcote.args 不允许覆盖安全敏感参数: --headless（请使用专用配置项）"]
        );
        assert_eq!(
            validate_browser_patch(&json!({"clearcote": {"args": ["--no-sandbox"]}})),
            vec![
                "clearcote.args 不允许直接写 --no-sandbox，请设置 browser.clearcote.sandbox: false"
            ]
        );
        assert_eq!(
            validate_browser_patch(&json!({"clearcote": {"args": [123]}})),
            vec!["clearcote.args 必须全是字符串，发现: number"]
        );
        assert_eq!(
            validate_browser_patch(&json!({"clearcote": {"camoufox": {}}})),
            vec!["clearcote 不允许 Firefox/Camoufox 专属字段: camoufox"]
        );

        assert_eq!(
            validate_pool_patch(&json!({"strategy": "nope"})),
            vec!["strategy 必须是 least_busy、round_robin 或 random"]
        );
        assert_eq!(
            validate_pool_patch(&json!({"failover": {"maxRetries": -1}})),
            vec!["failover.maxRetries 不能为负数"]
        );
        assert_eq!(
            validate_pool_patch(&json!({"failover": {"imgDlRetryMaxRetries": 99}})),
            vec!["failover.imgDlRetryMaxRetries 必须在 1-10 范围内"]
        );
        assert_eq!(
            validate_pool_patch(&json!({"strategy": "random"})),
            Vec::<String>::new()
        );

        assert_eq!(
            validate_adapters_patch(&json!({"gemini_biz": {"entryUrl": "http://x"}})),
            vec!["gemini_biz.entryUrl 必须以 https:// 开头"]
        );

        let adapters = vec!["lmarena".to_string(), "gemini".to_string()];
        assert_eq!(
            validate_instances_patch(&[], &adapters),
            vec!["instances 不能为空"]
        );
        let arr = |v: Value| v.as_array().cloned().unwrap_or_default();
        let bad = validate_instances_patch(
            &arr(json!([{
                "name": "i1",
                "workers": [{ "name": "w1", "type": "nope" }]
            }])),
            &adapters,
        );
        assert_eq!(
            bad,
            vec!["instances[0].workers[0]: type \"nope\" 不是有效的适配器类型"]
        );
        let dup = validate_instances_patch(
            &arr(json!([
                {"name": "i1", "workers": [{"name": "w1", "type": "lmarena"}]},
                {"name": "i2", "workers": [{"name": "w1", "type": "gemini"}]}
            ])),
            &adapters,
        );
        assert_eq!(
            dup,
            vec!["instances[1].workers[0]: Worker 名称 \"w1\" 全局重复（Worker 名称必须全局唯一）"]
        );
        let ok = validate_instances_patch(
            &arr(json!([{
                "name": "browser_default",
                "workers": [{ "name": "default", "type": "lmarena" }]
            }])),
            &adapters,
        );
        assert!(ok.is_empty(), "{ok:?}");
    }

    /// 适配器 ID 必须能从 src-root 真实扫到，否则配置校验会把合法 type 全部误判。
    /// 用真实原仓库路径验证（运行时的 src_root 由 --src-root / WEBAI2API_SRC_ROOT 提供）。
    #[test]
    fn adapter_ids_come_from_src_root() {
        // 运行时的 src_root 由 --src-root / WEBAI2API_SRC_ROOT 提供；测试允许显式指定，
        // 否则用相对 RS 项目目录的常见开发布局（../src/backend/adapter）。
        let candidates = [
            std::env::var("WEBAI2API_TEST_SRC_ROOT")
                .ok()
                .map(PathBuf::from),
            std::env::current_dir()
                .ok()
                .and_then(|d| d.parent().map(|p| p.to_path_buf())),
        ];
        let Some(root) = candidates
            .into_iter()
            .flatten()
            .find(|p| p.join("src/backend/adapter").is_dir())
        else {
            return; // 无原仓库可扫时跳过
        };
        let ids = adapter_ids_from_src(&root);
        assert!(
            ids.contains(&"lmarena".to_string()),
            "src-root={} ids={:?}",
            root.display(),
            ids
        );
        assert!(ids.contains(&"gemini".to_string()));
        assert!(!ids.iter().any(|i| i.contains('.')), "应已剥离 .js 后缀");
        // merge 是保留类型，不走适配器表
        let arr = |v: Value| v.as_array().cloned().unwrap_or_default();
        assert!(validate_instances_patch(
            &arr(json!([{
                "name": "i", "workers": [{ "name": "w", "type": "merge", "mergeTypes": ["lmarena"] }]
            }])),
            &ids
        )
        .is_empty());
    }
}
