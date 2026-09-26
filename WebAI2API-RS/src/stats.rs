//! 每日统计，对应原 `src/utils/stats.js`：`data/logs/stats_YYYY-MM-DD.json`，
//! 内容 `{"success":N,"failed":N}`，跨天自动落盘，每次计数整文件重写。
//! 文件 IO 放 tokio 阻塞线程池执行；当日内存计数仍由全局 Mutex 串行化，
//! 读写都在 blocking 闭包内短暂持锁，绝不跨 await。

use crate::errors::StatsError;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

struct StatsState {
    dir: PathBuf,
    date: String,
    success: u64,
    failed: u64,
}

fn state() -> &'static Mutex<StatsState> {
    static STATE: OnceLock<Mutex<StatsState>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(StatsState {
            dir: PathBuf::from("data/logs"),
            date: String::new(),
            success: 0,
            failed: 0,
        })
    })
}

fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

fn file_of(dir: &Path, date: &str) -> PathBuf {
    dir.join(format!("stats_{date}.json"))
}

fn read_file(path: &Path) -> (u64, u64) {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .map(|v| {
            (
                v.get("success").and_then(Value::as_u64).unwrap_or(0),
                v.get("failed").and_then(Value::as_u64).unwrap_or(0),
            )
        })
        .unwrap_or((0, 0))
}

fn write_file(dir: &Path, date: &str, success: u64, failed: u64) {
    let _ = fs::create_dir_all(dir);
    let body =
        serde_json::to_string_pretty(&json!({"success": success, "failed": failed})).unwrap();
    let _ = fs::write(file_of(dir, date), body + "\n");
}

pub fn init(log_dir: &Path) {
    let mut s = state().lock().unwrap();
    s.dir = log_dir.to_path_buf();
    s.date = today();
    let (ok, bad) = read_file(&file_of(&s.dir, &s.date));
    s.success = ok;
    s.failed = bad;
}

fn rollover(s: &mut StatsState) {
    let now = today();
    if s.date.is_empty() {
        s.date = now;
        return;
    }
    if s.date != now {
        write_file(&s.dir, &s.date, s.success, s.failed);
        s.date = now;
        let (ok, bad) = read_file(&file_of(&s.dir, &s.date));
        s.success = ok;
        s.failed = bad;
    }
}

pub async fn increment_success() {
    let _ = tokio::task::spawn_blocking(|| bump(true)).await;
}
pub async fn increment_failed() {
    let _ = tokio::task::spawn_blocking(|| bump(false)).await;
}

fn bump(success: bool) {
    let mut s = state().lock().unwrap();
    rollover(&mut s);
    if success {
        s.success += 1;
    } else {
        s.failed += 1;
    }
    write_file(&s.dir, &s.date, s.success, s.failed);
}

fn today_stats_sync() -> (u64, u64) {
    let mut s = state().lock().unwrap();
    rollover(&mut s);
    (s.success, s.failed)
}

pub async fn today_stats() -> (u64, u64) {
    // JoinError 仅在闭包 panic 时出现（正常不可达），回退为 0 计数
    tokio::task::spawn_blocking(today_stats_sync)
        .await
        .unwrap_or((0, 0))
}

/// 区间累加。日期键沿用 JS 版的 UTC `toISOString` 语义（stats.js getStatsRange）。
pub async fn range(start: &str, end: &str) -> Result<(u64, u64, u64), StatsError> {
    let start = start.to_string();
    let end = end.to_string();
    tokio::task::spawn_blocking(move || range_sync(&start, &end))
        .await
        .unwrap_or_else(|e| Err(e.into()))
}

fn range_sync(start: &str, end: &str) -> Result<(u64, u64, u64), StatsError> {
    let start_date = chrono::NaiveDate::parse_from_str(start, "%Y-%m-%d")
        .map_err(|_| StatsError::InvalidStartDate)?;
    let end_date = chrono::NaiveDate::parse_from_str(end, "%Y-%m-%d")
        .map_err(|_| StatsError::InvalidEndDate)?;
    let dir = state().lock().unwrap().dir.clone();
    let mut success = 0;
    let mut failed = 0;
    let mut days = 0;
    let mut d = start_date;
    while d <= end_date {
        let key = d.format("%Y-%m-%d").to_string();
        let (ok, bad) = read_file(&file_of(&dir, &key));
        if ok + bad > 0 {
            days += 1;
        }
        success += ok;
        failed += bad;
        d += chrono::Duration::days(1);
    }
    Ok((success, failed, days))
}

pub async fn clear_range(start: &str, end: &str) -> Result<u64, StatsError> {
    let start = start.to_string();
    let end = end.to_string();
    tokio::task::spawn_blocking(move || clear_range_sync(&start, &end))
        .await
        .unwrap_or_else(|e| Err(e.into()))
}

fn clear_range_sync(start: &str, end: &str) -> Result<u64, StatsError> {
    let start_date = chrono::NaiveDate::parse_from_str(start, "%Y-%m-%d")
        .map_err(|_| StatsError::InvalidStartDate)?;
    let end_date = chrono::NaiveDate::parse_from_str(end, "%Y-%m-%d")
        .map_err(|_| StatsError::InvalidEndDate)?;
    let mut s = state().lock().unwrap();
    let mut removed = 0;
    let mut d = start_date;
    while d <= end_date {
        let key = d.format("%Y-%m-%d").to_string();
        if fs::remove_file(file_of(&s.dir, &key)).is_ok() {
            removed += 1;
        }
        if key == s.date {
            s.success = 0;
            s.failed = 0;
        }
        d += chrono::Duration::days(1);
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn persist_and_range() {
        let dir = std::env::temp_dir().join(format!("webai2api-stats-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        init(&dir);
        increment_success().await;
        increment_failed().await;
        let (ok, bad) = today_stats().await;
        assert_eq!((ok, bad), (1, 1));
        let key = today();
        let (s, f, days) = range(&key, &key).await.unwrap();
        assert_eq!((s, f, days), (1, 1, 1));
        assert!(file_of(&dir, &key).exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
