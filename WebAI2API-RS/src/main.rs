//! WebAI2API Rust 版入口：参数解析、单实例锁，其余编排见 lib 的 run 模块。
//!
//! 退出码契约与原版一致：78 = 配置/端口/单实例锁等致命错误（不重启）；
//! 其他非零 = 可自动重启，上限 3 次。

use std::path::PathBuf;
use webai2api_rs::instance_lock::acquire_lock;
use webai2api_rs::logfmt;
use webai2api_rs::run::{self, StartOptions};

fn parse_args() -> StartOptions {
    let mut opts = StartOptions::default();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "-xvfb" => opts.xvfb = true,
            "-vnc" => opts.vnc = true,
            "--data-dir" => opts.data_dir = it.next().map(PathBuf::from).unwrap_or_default(),
            "--src-root" => opts.src_root = it.next().map(PathBuf::from).unwrap_or_default(),
            s if s.starts_with("-login=") => opts.login = Some(s["-login=".len()..].to_string()),
            "-login" => opts.login = Some(String::new()),
            // 内部探针：供单实例锁单测验证跨进程互斥，不属于对外 CLI
            "--lock-probe" => {
                let dir = it.next().map(PathBuf::from).unwrap_or_default();
                let locked_elsewhere = acquire_lock(&dir).is_err();
                println!(
                    "{}",
                    if locked_elsewhere {
                        "LOCKED-ELSEWHERE"
                    } else {
                        "ACQUIRED"
                    }
                );
                std::process::exit(0);
            }
            _ => {}
        }
    }
    if opts.src_root.as_os_str().is_empty() {
        opts.src_root = std::env::current_dir().unwrap_or_default();
    }
    if opts.data_dir.as_os_str().is_empty() {
        opts.data_dir = opts.src_root.join("data");
    }
    opts
}

#[tokio::main]
async fn main() {
    let opts = parse_args();

    logfmt::init(&opts.data_dir.join("logs"), "info");
    logfmt::info("看门狗", "主进程已启动");

    // 单实例锁：旧进程仍存活则拒绝启动（supervisor.js 同款语义，exit 78）
    if let Err(e) = acquire_lock(&opts.data_dir) {
        logfmt::error("看门狗", &e.to_string());
        std::process::exit(run::FATAL_EXIT);
    }

    run::watchdog(opts).await
}
