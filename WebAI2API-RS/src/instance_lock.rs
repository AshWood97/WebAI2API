//! 单实例锁：Unix 用 flock 独占（进程死亡自动释放，堵住 check-then-write 竞态），
//! 锁文件内容为 PID 便于诊断；非 Unix 回退到 PID 存活检测 + 覆盖写。
//! 语义对应原 supervisor.js 的 data/.webai2api-supervisor.lock。

use crate::errors::LockError;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

static LOCK_KEEPALIVE: OnceLock<Mutex<Option<std::fs::File>>> = OnceLock::new();

pub fn lock_path(data_dir: &Path) -> PathBuf {
    data_dir.join(".webai2api-supervisor.lock")
}

/// 拿到单实例锁；已有存活实例持锁时返回 Err（调用方以退出码 78 终止）。
pub fn acquire_lock(data_dir: &Path) -> Result<(), LockError> {
    std::fs::create_dir_all(data_dir).map_err(|e| LockError::CreateDataDir { source: e })?;
    let lock = lock_path(data_dir);

    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::io::AsRawFd;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock)
            .map_err(|e| LockError::LockCreate { source: e })?;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let pid = std::fs::read_to_string(&lock)
                .ok()
                .and_then(|s| s.trim().parse::<i32>().ok())
                .unwrap_or(0);
            return Err(LockError::AlreadyRunning { pid });
        }
        file.set_len(0)
            .map_err(|e| LockError::LockWrite { source: e })?;
        write!(file, "{}", std::process::id()).map_err(|e| LockError::LockWrite { source: e })?;
        // File 必须保活到进程结束，flock 才持续有效
        let _ = LOCK_KEEPALIVE
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap()
            .insert(file);
        Ok(())
    }

    #[cfg(not(unix))]
    {
        if let Ok(old) = std::fs::read_to_string(&lock) {
            if let Ok(pid) = old.trim().parse::<i32>() {
                if pid != std::process::id() as i32 && process_alive(pid) {
                    return Err(LockError::AlreadyRunning { pid });
                }
            }
        }
        std::fs::write(&lock, std::process::id().to_string())
            .map_err(|e| LockError::LockCreate { source: e })?;
        Ok(())
    }
}

/// 清理锁文件（仅当内容仍是本进程 PID；flock 已随进程释放，这里只删文件）。
pub fn release_lock(data_dir: &Path) {
    let lock = lock_path(data_dir);
    let my = std::process::id().to_string();
    if std::fs::read_to_string(&lock)
        .ok()
        .as_deref()
        .map(str::trim)
        == Some(my.as_str())
    {
        let _ = std::fs::remove_file(&lock);
    }
}

#[cfg(not(unix))]
fn process_alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("webai2api-lock-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn acquire_release_rewrites_lock_file() {
        let dir = temp_dir("basic");
        acquire_lock(&dir).expect("首次获取应成功");
        assert_eq!(
            std::fs::read_to_string(lock_path(&dir)).unwrap().trim(),
            std::process::id().to_string()
        );
        release_lock(&dir);
        assert!(!lock_path(&dir).exists(), "release 后锁文件应删除");
        // 释放后可重新获取（跨进程互斥见 tests/integration.rs 的 lock_cross_process）
        acquire_lock(&dir).expect("释放后重新获取应成功");
        release_lock(&dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_lock_from_dead_pid_is_overwritten() {
        let dir = temp_dir("stale");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(lock_path(&dir), "999999999").unwrap();
        acquire_lock(&dir).expect("陈旧锁（死 PID）应被覆盖获取");
        assert_eq!(
            std::fs::read_to_string(lock_path(&dir)).unwrap().trim(),
            std::process::id().to_string()
        );
        release_lock(&dir);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
