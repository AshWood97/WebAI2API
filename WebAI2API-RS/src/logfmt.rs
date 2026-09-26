//! 日志，格式与轮转逐条对应原 `src/utils/logger.js`。
//! 行格式：`YYYY-MM-DD HH:mm:ss.SSS [LEVL] [模块] [id] [adapter] [model] 消息 | k=v`
//! 轮转：`data/logs/system.log` 达到 5MB 时改名为 `system.log.old`（只保留两代）。

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const MAX_SIZE: u64 = 5 * 1024 * 1024;
const LEVELS: [(&str, &str, u8); 4] = [
    ("debug", "DBUG", 0),
    ("info", "INFO", 1),
    ("warn", "WARN", 2),
    ("error", "ERRO", 3),
];

struct LoggerState {
    path: PathBuf,
    old_path: PathBuf,
    min_level: u8,
    /// 持久句柄：避免每条日志 open/close。轮转或路径变更后置 None，下次写入时重建。
    file: Option<File>,
}

fn state() -> &'static Mutex<LoggerState> {
    static STATE: OnceLock<Mutex<LoggerState>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(LoggerState {
            path: PathBuf::from("data/logs/system.log"),
            old_path: PathBuf::from("data/logs/system.log.old"),
            min_level: 1,
            file: None,
        })
    })
}

/// 设置日志目录与最低等级（debug/info/warn/error）。
pub fn init(log_dir: &Path, level: &str) {
    let mut s = state().lock().unwrap();
    s.path = log_dir.join("system.log");
    s.old_path = log_dir.join("system.log.old");
    s.min_level = level_rank(level);
    // 路径变更后旧句柄指向旧文件，置 None 让下一次写入重新打开
    s.file = None;
}

fn level_rank(level: &str) -> u8 {
    LEVELS
        .iter()
        .find(|(n, _, _)| *n == level)
        .map(|(_, _, r)| *r)
        .unwrap_or(1)
}

fn level_tag(level: &str) -> &'static str {
    LEVELS
        .iter()
        .find(|(n, _, _)| *n == level)
        .map(|(_, t, _)| *t)
        .unwrap_or("INFO")
}

/// 写一条日志。`front` 为前置方括号字段（id/adapter/model），`meta` 为 `| k=v` 尾部。
pub fn log(
    level: &str,
    module: &str,
    message: &str,
    front: &[(&str, &str)],
    meta: &[(&str, &str)],
) {
    if level_rank(level) < state().lock().unwrap().min_level {
        return;
    }
    let line = format_line(level, module, message, front, meta);

    // 控制台着色与 JS 版一致：error 红、warn 黄、info 白、debug 蓝
    let color = match level {
        "error" => "31",
        "warn" => "33",
        "debug" => "34",
        _ => "37",
    };
    let console_line = format!("\x1b[{color}m{line}\x1b[0m");
    match level {
        "error" => eprintln!("{console_line}"),
        "warn" => eprintln!("{console_line}"),
        _ => println!("{console_line}"),
    }

    let mut s = state().lock().unwrap();
    if let Some(parent) = s.path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if rotate_if_needed(&s.path, &s.old_path) {
        // 轮转后旧句柄指向被改名的文件，必须重新打开新文件
        s.file = None;
    }
    if s.file.is_none() {
        s.file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&s.path)
            .ok();
    }
    let written = s
        .file
        .as_mut()
        .is_some_and(|f| writeln!(f, "{line}").is_ok());
    if !written {
        // 句柄失效（如文件被外部删除）时重建一次，对齐旧版逐条 open 的行为
        s.file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&s.path)
            .ok();
        if let Some(f) = s.file.as_mut() {
            let _ = writeln!(f, "{line}");
        }
    }
}

pub fn info(module: &str, message: &str) {
    log("info", module, message, &[], &[]);
}
pub fn warn(module: &str, message: &str) {
    log("warn", module, message, &[], &[]);
}
pub fn error(module: &str, message: &str) {
    log("error", module, message, &[], &[]);
}
pub fn debug(module: &str, message: &str) {
    log("debug", module, message, &[], &[]);
}

/// 纯格式化（可单测，不触碰 IO）。
pub fn format_line(
    level: &str,
    module: &str,
    message: &str,
    front: &[(&str, &str)],
    meta: &[(&str, &str)],
) -> String {
    let now = chrono::Local::now();
    let ts = now.format("%Y-%m-%d %H:%M:%S%.3f");
    let msg = message.replace('\n', " ↵ ");
    let mut line = format!("{ts} [{}] [{module}]", level_tag(level));
    for (_, v) in front {
        if !v.is_empty() {
            line.push_str(&format!(" [{v}]"));
        }
    }
    line.push(' ');
    line.push_str(&msg);
    let tail: Vec<String> = meta
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    if !tail.is_empty() {
        line.push_str(" | ");
        line.push_str(&tail.join(" "));
    }
    line
}

/// 达到 5MB 时把当前文件改名到 `.old`，返回是否发生了轮转。
fn rotate_if_needed(path: &Path, old: &Path) -> bool {
    if let Ok(meta) = fs::metadata(path) {
        if meta.len() >= MAX_SIZE {
            let _ = fs::remove_file(old);
            return fs::rename(path, old).is_ok();
        }
    }
    false
}

pub fn log_path() -> PathBuf {
    state().lock().unwrap().path.clone()
}

/// 读取最后 n 行（admin /logs）。返回 (行, 总行数, 文件路径)。
pub fn read_tail(n: usize) -> (Vec<String>, usize, String) {
    let path = log_path();
    let content = fs::read_to_string(&path).unwrap_or_default();
    let lines: Vec<String> = content
        .lines()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string())
        .collect();
    let total = lines.len();
    let tail = lines
        .into_iter()
        .rev()
        .take(n)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    (tail, total, path.display().to_string())
}

pub fn clear() {
    let mut s = state().lock().unwrap();
    // 先丢弃句柄再删文件：否则后续写入会进入孤儿 inode，文件不会重新出现
    s.file = None;
    let _ = fs::remove_file(&s.path);
    let _ = fs::remove_file(&s.old_path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_format() {
        let line = format_line(
            "error",
            "服务器",
            "失败\n重试",
            &[("id", "abc")],
            &[("error", "boom")],
        );
        assert!(
            line.contains("[ERRO] [服务器] [abc] 失败 ↵ 重试 | error=boom"),
            "{line}"
        );
        let d = format_line("debug", "池", "x", &[], &[]);
        assert!(d.contains("[DBUG] [池] x"));
    }

    #[test]
    fn rotation_threshold() {
        let dir = std::env::temp_dir().join(format!("webai2api-logfmt-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("system.log");
        let old = dir.join("system.log.old");
        fs::write(&path, vec![b'a'; MAX_SIZE as usize]).unwrap();
        rotate_if_needed(&path, &old);
        assert!(old.exists());
        assert!(!path.exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
