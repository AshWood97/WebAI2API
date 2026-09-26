//! 系统指标采集（/admin/status、/v1/runtime/status 用）。
//! 字段名与 `stores/system.js` 的 state 一致，内存单位 MB；macOS 的 free 口径
//! 对齐 Node os.freemem()（vm.page_free_count）。

use serde_json::{json, Value};
use std::sync::Mutex;

use crate::server::{AppState, VERSION};

/// `/admin/status` 的 status/version/systemVersion/uptime/cpuUsage/memoryUsage。
pub fn system_status(state: &AppState) -> Value {
    let mem = sysinfo_mem();
    let mode = if std::env::var("XVFB_RUNNING").is_ok() {
        "xvfb"
    } else if std::env::var("HEADLESS").ok().as_deref() == Some("true") {
        "headless"
    } else {
        "normal"
    };
    json!({
        "status": mode,
        "version": VERSION,
        "systemVersion": format!("{} {}", os_name(), os_release()),
        "uptime": state.started_at.elapsed().as_secs(),
        "cpuUsage": cpu_usage_percent(),
        "memoryUsage": mem,
    })
}

fn os_name() -> &'static str {
    match std::env::consts::OS {
        "macos" => "Darwin",
        "linux" => "Linux",
        "windows" => "Windows_NT",
        other => other,
    }
}

fn os_release() -> String {
    // 内核版本进程内不变：首次读 /proc，失败回退 uname -r，结果缓存
    static CACHE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            std::fs::read_to_string("/proc/sys/kernel/osrelease")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    std::process::Command::new("uname")
                        .arg("-r")
                        .output()
                        .ok()
                        .filter(|o| o.status.success())
                        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                        .filter(|s| !s.is_empty())
                })
                .unwrap_or_else(|| "unknown".to_string())
        })
        .clone()
}

/// 总内存（hw.memsize）进程内不变；动态内存值（vm.page_free_count）不缓存。
fn total_memory_bytes() -> u64 {
    static CACHE: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| sysctl_u64("hw.memsize").unwrap_or(0))
}

fn sysinfo_mem() -> Value {
    let page = page_size();
    let (total, free) = match std::env::consts::OS {
        // 对齐 Node os.freemem()：macOS 上取 vm.page_free_count（host_statistics64 的 free_count），
        // 不含 purgeable/speculative/compressor，数值明显小于"可用内存"的宽松口径。
        "macos" => {
            let total = total_memory_bytes();
            (
                total,
                sysctl_u64("vm.page_free_count")
                    .unwrap_or(0)
                    .saturating_mul(page),
            )
        }
        "linux" => linux_mem(),
        _ => (0, 0),
    };
    mb_usage(total, free)
}

pub fn mb_usage(total: u64, free: u64) -> Value {
    let free = free.min(total);
    json!({
        "total": total / 1024 / 1024,
        "used": total.saturating_sub(free) / 1024 / 1024,
        "free": free / 1024 / 1024,
    })
}

fn page_size() -> u64 {
    #[cfg(unix)]
    {
        unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(0) as u64
    }
    #[cfg(not(unix))]
    {
        4096
    }
}

fn sysctl_u64(name: &str) -> Option<u64> {
    let out = std::process::Command::new("sysctl")
        .args(["-n", name])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

fn linux_mem() -> (u64, u64) {
    let text = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let kb = |key: &str| {
        text.lines()
            .find_map(|line| {
                let rest = line.strip_prefix(key)?.trim();
                rest.split_whitespace().next()?.parse::<u64>().ok()
            })
            .unwrap_or(0)
            * 1024
    };
    let total = kb("MemTotal:");
    let available = kb("MemAvailable:");
    (
        total,
        if available > 0 {
            available
        } else {
            kb("MemFree:")
        },
    )
}

/// 两次采样之间的 CPU 使用率，一位小数。首次调用返回 0。
fn cpu_usage_percent() -> f64 {
    let now = cpu_ticks();
    let mut prev = CPU_SAMPLE.lock().unwrap();
    let usage = match *prev {
        Some((idle0, total0)) if now.1 > total0 => {
            let idle = now.0.saturating_sub(idle0) as f64;
            let total = (now.1 - total0) as f64;
            if total == 0.0 {
                0.0
            } else {
                ((100.0 - idle / total * 100.0) * 10.0).round() / 10.0
            }
        }
        _ => 0.0,
    };
    *prev = Some(now);
    usage
}

fn cpu_ticks() -> (u64, u64) {
    if let Ok(text) = std::fs::read_to_string("/proc/stat") {
        if let Some(line) = text.lines().next() {
            let nums: Vec<u64> = line
                .split_whitespace()
                .skip(1)
                .filter_map(|s| s.parse().ok())
                .collect();
            if nums.len() >= 4 {
                let idle = nums[3] + nums.get(4).copied().unwrap_or(0);
                return (idle, nums.iter().sum());
            }
        }
    }
    // macOS：host_processor_info 的累计 CPU 时间。首次为 0，之后按差分。
    macos_cpu_ticks().unwrap_or((0, 0))
}

fn macos_cpu_ticks() -> Option<(u64, u64)> {
    #[cfg(target_os = "macos")]
    {
        use std::os::raw::{c_int, c_uint};
        #[repr(C)]
        struct ProcessorInfo {
            cpu_ticks: [c_uint; 4],
        }
        unsafe extern "C" {
            fn host_processor_info(
                host: c_uint,
                flavor: c_int,
                out_count: *mut c_uint,
                info: *mut *mut c_int,
                out_info_count: *mut c_uint,
            ) -> c_int;
            fn vm_deallocate(target: c_uint, address: usize, size: usize) -> c_int;
            fn mach_host_self() -> c_uint;
        }
        const CPU_STATE_MAX: usize = 4;
        const CPU_STATE_IDLE: usize = 2;
        const PROCESSOR_CPU_LOAD_INFO: c_int = 2;
        let host = unsafe { mach_host_self() };
        let mut cpu_count: c_uint = 0;
        let mut info: *mut c_int = std::ptr::null_mut();
        let mut info_count: c_uint = 0;
        let rc = unsafe {
            host_processor_info(
                host,
                PROCESSOR_CPU_LOAD_INFO,
                &mut cpu_count,
                &mut info,
                &mut info_count,
            )
        };
        if rc != 0 || info.is_null() || cpu_count == 0 {
            return None;
        }
        let ticks =
            unsafe { std::slice::from_raw_parts(info as *const ProcessorInfo, cpu_count as usize) };
        let mut idle = 0u64;
        let mut total = 0u64;
        for cpu in ticks {
            for i in 0..CPU_STATE_MAX {
                total += cpu.cpu_ticks[i] as u64;
            }
            idle += cpu.cpu_ticks[CPU_STATE_IDLE] as u64;
        }
        unsafe {
            vm_deallocate(
                host,
                info as usize,
                info_count as usize * std::mem::size_of::<c_int>(),
            );
        }
        Some((idle, total))
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

static CPU_SAMPLE: Mutex<Option<(u64, u64)>> = Mutex::new(None);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_usage_clamps_free_to_total() {
        let usage = mb_usage(2048 * 1024 * 1024, 4096 * 1024 * 1024);
        assert_eq!(usage["total"], 2048);
        assert_eq!(usage["used"], 0);
        assert_eq!(usage["free"], 2048);
    }
}
