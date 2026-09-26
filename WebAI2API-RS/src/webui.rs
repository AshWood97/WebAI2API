//! WebUI 服务面：静态资源分发（SPA fallback + 穿越防护）与 VNC WebSocket 代理。

use crate::config;
use crate::server::{AppState, WS_GUID};
use axum::body::Body;
use axum::extract::ws;
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::Response;
use base64::Engine;
use serde::Deserialize;
use serde_json::Value;
use sha1::{Digest, Sha1};
use std::path::Path;
use std::sync::Arc;

// ==================== 静态资源 ====================

pub async fn serve_static(state: &AppState, path: &str) -> Option<Response> {
    let rel = if path == "/" {
        "index.html".to_string()
    } else {
        path.trim_start_matches('/').to_string()
    };
    let webui_dir = state.webui_dir.clone();
    // is_file/canonicalize/fs::read 都是文件系统调用，放阻塞线程池执行。
    // Ok(Some(Err(()))) 表示穿越防护命中（403），None 表示未命中静态资源。
    let loaded = tokio::task::spawn_blocking(move || {
        let full = webui_dir.join(&rel);
        // 穿越防护：解析后的路径必须仍在 webui 目录内
        let candidate = if full.is_file() {
            full
        } else {
            webui_dir.join("index.html")
        };
        let canon = candidate.canonicalize().ok()?;
        let base = webui_dir.canonicalize().ok()?;
        if !canon.starts_with(&base) {
            return Some(Err(()));
        }
        let bytes = std::fs::read(&canon).ok()?;
        let mime = mime_of(&canon);
        Some(Ok((mime, bytes)))
    })
    .await
    .ok()??;
    match loaded {
        Err(()) => Some(
            Response::builder()
                .status(403)
                .body(Body::from("Forbidden"))
                .unwrap(),
        ),
        Ok((mime, bytes)) => Some(
            Response::builder()
                .status(200)
                .header(header::CONTENT_TYPE, mime)
                .body(Body::from(bytes))
                .unwrap(),
        ),
    }
}

fn mime_of(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "application/javascript; charset=utf-8",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        _ => "application/octet-stream",
    }
}

// ==================== VNC WebSocket 代理 ====================

pub async fn vnc_ws(
    State(state): State<Arc<AppState>>,
    ws: ws::WebSocketUpgrade,
    Query(q): Query<TokenQuery>,
) -> Response {
    let auth = config::auth_of(&state.config);
    if !auth.is_empty() && !crate::server::token_eq(q.token.as_deref().unwrap_or(""), &auth) {
        return Response::builder()
            .status(401)
            .body(Body::from("Unauthorized"))
            .unwrap();
    }
    let vnc = state.vnc.lock().unwrap().clone();
    if !vnc.enabled {
        return Response::builder()
            .status(503)
            .body(Body::from("VNC 未启用"))
            .unwrap();
    }
    let port = vnc.port;
    ws.on_upgrade(move |socket| proxy_vnc(socket, port))
}

#[derive(Deserialize)]
pub struct TokenQuery {
    token: Option<String>,
}

async fn proxy_vnc(mut socket: ws::WebSocket, port: u16) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut tcp = match tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
        Ok(s) => s,
        Err(_) => return,
    };
    let (mut reader, mut writer) = tcp.split();
    let mut buf = vec![0u8; 65536];
    loop {
        tokio::select! {
            msg = socket.recv() => match msg {
                Some(Ok(ws::Message::Binary(data))) => { if writer.write_all(&data).await.is_err() { break; } }
                Some(Ok(ws::Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
            n = reader.read(&mut buf) => match n {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if socket.send(ws::Message::Binary(buf[..n].to_vec().into())).await.is_err() { break; }
                }
            },
        }
    }
}

/// 手写握手用的 accept 值（单测对照 RFC6455；实际握手由 axum 完成）。
pub fn ws_accept_key(sec_key: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(sec_key.as_bytes());
    hasher.update(WS_GUID.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(hasher.finalize())
}

// ==================== 数据目录安全 ====================

pub fn managed_folder(name: &str) -> bool {
    name == "camoufoxUserData"
        || name == "clearcoteUserData"
        || (name
            .strip_prefix("camoufoxUserData_")
            .is_some_and(valid_mark))
        || (name
            .strip_prefix("clearcoteUserData_")
            .is_some_and(valid_mark))
}

fn valid_mark(mark: &str) -> bool {
    !mark.is_empty()
        && mark
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub fn profile_data_dir(state: &AppState) -> std::path::PathBuf {
    state
        .config
        .pointer("/backend/pool/workers")
        .and_then(Value::as_array)
        .and_then(|ws| {
            ws.iter()
                .find_map(|w| w.get("userDataDir").and_then(Value::as_str))
        })
        .map(Path::new)
        .and_then(|dir| dir.parent())
        .filter(|dir| dir.is_dir())
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| state.data_dir.clone())
}

pub fn data_folders(data_dir: &Path, config: &Value) -> Value {
    let mut folders = Vec::new();
    if let Ok(entries) = std::fs::read_dir(data_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !managed_folder(&name) {
                continue;
            }
            // 这台 Rust 的 DirEntry::metadata 不跟随符号链接，必须用路径判断
            if !entry.path().is_dir() {
                continue;
            }
            let size = dir_size(&entry.path(), 3);
            let instance = config
                .pointer("/backend/pool/workers")
                .and_then(Value::as_array)
                .and_then(|ws| {
                    ws.iter().find(|w| {
                        w["userDataDir"]
                            .as_str()
                            .is_some_and(|d| d.ends_with(&name))
                    })
                })
                .and_then(|w| w["name"].as_str());
            let engine = if name.starts_with("clearcoteUserData") {
                "clearcote"
            } else {
                "camoufox"
            };
            folders.push(serde_json::json!({
                "name": name,
                "path": format!("data/{name}"),
                "size": format_size(size),
                "sizeBytes": size,
                "engine": engine,
                "instance": instance,
            }));
        }
    }
    Value::Array(folders)
}

pub fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < MB {
        format!("{:.1} KB", b / KB)
    } else if b < GB {
        format!("{:.1} MB", b / MB)
    } else {
        format!("{:.1} GB", b / GB)
    }
}

/// 删除托管数据目录。返回 (HTTP 状态, body)：部分失败为 207。
pub fn delete_data_folders(data_dir: &Path, names: &[String]) -> (u16, Value) {
    let mut deleted = Vec::new();
    let mut errors = Vec::new();
    for name in names {
        if !managed_folder(name) {
            errors.push(serde_json::json!({"name": name, "error": "不在托管目录名单内"}));
            continue;
        }
        let target = data_dir.join(name);
        match (target.canonicalize(), data_dir.canonicalize()) {
            (Ok(canon), Ok(base))
                if canon.starts_with(&base) && canon != base && canon.is_dir() =>
            {
                match std::fs::remove_dir_all(&canon) {
                    Ok(()) => deleted.push(serde_json::json!(name)),
                    Err(e) => {
                        errors.push(serde_json::json!({"name": name, "error": e.to_string()}))
                    }
                }
            }
            _ => errors.push(serde_json::json!({"name": name, "error": "路径越界或不是目录"})),
        }
    }
    let status = if errors.is_empty() {
        200
    } else if deleted.is_empty() {
        400
    } else {
        207
    };
    (
        status,
        serde_json::json!({"success": errors.is_empty(), "deleted": deleted, "errors": errors}),
    )
}

pub fn dir_size(path: &Path, depth: u8) -> u64 {
    if depth == 0 {
        return 0;
    }
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let meta = entry.metadata().ok();
            if meta.as_ref().is_some_and(|m| m.is_file()) {
                total += meta.unwrap().len();
            } else if meta.is_some_and(|m| m.is_dir()) {
                total += dir_size(&entry.path(), depth - 1);
            }
        }
    }
    total
}

pub fn clear_dir(dir: &Path) -> usize {
    let mut n = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.path().is_file() && std::fs::remove_file(entry.path()).is_ok() {
                n += 1;
            }
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_accept_matches_rfc() {
        // RFC6455 文档示例：dGhlIHNhbXBsZSBub25jZQ== → s3pPLMBiTxaQ9kYGzzhZRbK+xOo=
        assert_eq!(
            ws_accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn managed_folder_rules() {
        assert!(managed_folder("camoufoxUserData"));
        assert!(managed_folder("clearcoteUserData_a-1"));
        assert!(!managed_folder("clearcoteUserDataEvil"));
        assert!(!managed_folder("camoufoxUserData_../x"));
        assert!(!managed_folder("other"));
    }

    #[test]
    fn symlink_dir_is_visible_to_metadata() {
        let dir = std::env::temp_dir().join(format!("webai2api-linkmeta-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let target = dir.join("target");
        std::fs::create_dir_all(&target).unwrap();
        let link = dir.join("camoufoxUserData");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let entry = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .find(|e| e.file_name() == "camoufoxUserData")
            .unwrap();
        assert!(
            entry.path().is_dir(),
            "path::is_dir follows the directory symlink"
        );
        assert!(link.is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_folders_guards() {
        let dir =
            std::env::temp_dir().join(format!("webai2api-datafolders-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("camoufoxUserData")).unwrap();
        std::fs::write(dir.join("camoufoxUserData").join("x"), b"1").unwrap();
        let (status, body) = delete_data_folders(&dir, &["../etc".into(), "nope".into()]);
        assert_eq!(status, 400);
        assert_eq!(body["deleted"].as_array().unwrap().len(), 0);
        let (status, body) = delete_data_folders(&dir, &["camoufoxUserData".into()]);
        assert_eq!(status, 200);
        assert!(!dir.join("camoufoxUserData").exists(), "{}", body);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn format_sizes_match_webui() {
        assert_eq!(format_size(100), "100 B");
        assert_eq!(format_size(1536), "1.5 KB");
    }
}
