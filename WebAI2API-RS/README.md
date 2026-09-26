# WebAI2API-RS

WebAI2API 的 Rust 重写。HTTP 服务、队列、配置、历史记录、统计、日志和进程管理
全部由一个 Rust 二进制承担；浏览器驱动层（camoufox / clearcote 两个引擎及其
19 个站点适配器）保持原样，由一个 Node 引擎桥子进程运行。

原仓库一行未改，作为对照与备份。本目录只新增文件。

## 架构

```
webai2api (Rust)
  ├─ 单实例锁（flock，进程死亡自动释放）、崩溃重启（退出码 78 不重启，其余上限 3 次）
  ├─ Linux 下按需拉起 Xvfb / x11vnc（scripts/start-xvfb.sh、start-vnc.sh），
  │  子进程退出有监控：Xvfb 退出触发整轮重启，x11vnc 退出只置 VNC 不可用
  ├─ axum：OpenAI / Anthropic / Admin API、SSE、WebUI 静态资源、VNC WebSocket 代理
  └─ Unix socket JSON 行协议 ↓
bridge/bridge.mjs (Node)
  └─ 直接 import 原仓库 src/backend（adapter / engine / pool），不复制代码
```

桥 spawn 时 setsid 自成进程组：停机时整组击杀， camoufox 等孙进程不会成为孤儿。
桥脚本以 `WEBAI2API_TEMP_DIR`（默认 `--data-dir/temp`）落生成图片，`/admin/cache/clear` 可覆盖。

桥协议方法：`preflight`、`init`、`generate`、`getModels`、`modelInfo`
（模型表 + 单模型 type/policy 的聚合查询，Rust 侧带缓存）、`getImagePolicy`、
`getModelType`、`getCookies`、`navigateToMonitor`、`listAdapters`、
`downloadViaContext`、`restartBrowser`、`shutdown`。生成结果里的图片走临时文件，不经过 socket。

停机路径：`/admin/stop`、IPC `STOP`、Ctrl-C、SIGTERM 都收敛到同一条清理通道
（桥 shutdown → 杀进程组 → 清单实例锁），不存在绕过浏览器清理的退出路径。

## 构建与运行

```bash
cd WebAI2API-RS
cargo build --release

# 在原仓库根目录运行，直接复用 data/、camoufox/、node_modules/
./target/release/webai2api --src-root .. --data-dir ../data

# 登录模式、虚拟显示与原版参数一致
./target/release/webai2api --src-root .. -login=workerName
./target/release/webai2api --src-root .. -xvfb -vnc      # 仅 Linux
```

生成密钥：`./target/release/webai2api-genkey`，输出 `sk-` 加 48 位十六进制。

运行要求：Rust 1.85+、Node.js 20+，以及原仓库已安装的依赖
（`camoufox-js`、`clearcote`、`playwright-core` 等）。浏览器内核仍由
`npm run init` 下载到原仓库的 `camoufox/` 目录。

## 兼容性

- 直接读取原仓库的 `data/config.yaml`，默认值、校验文案、退出码与原版一致。
- `data/history/history.db` 表结构不变，可直接打开原有数据库。
- WebUI 使用原仓库 `webui/dist` 的构建产物（`webui-dist` 符号链接）。
- 保留原版的已知行为：非流式 429 文案里的队列上限显示为 `undefined`、
  `/v1/chat/completions` 前缀匹配、流式请求不受队列上限约束。
- 配置写回是合并式的：只更新提交的键，其余键保留。YAML 注释仍会在写回时丢失，
  与原版 `yaml.stringify` 的行为相同；写回是原子的（临时文件 + rename），
  加载器校验失败会回滚原文。

## 测试

```bash
cargo test            # 42 个测试：单元 + 集成（mock 引擎桥驱动完整服务）
```

集成测试在进程内用 `bridge/mock-bridge.mjs` 起完整服务，覆盖：端点与生成链路、
安全模式 503、桥崩溃自动重启、非流式 429、flock 跨进程互斥、SIGTERM 优雅停机。
不需要浏览器内核。需要 Node 可执行文件在 PATH 中（测试 spawn mock 桥）。

CI：`.github/workflows/rust-ci.yml` 运行 `cargo fmt --check`、
`cargo clippy --all-targets -- -D warnings`、`cargo test`、release 构建与版本一致性检查。

## 目录

| 路径 | 内容 |
|---|---|
| `src/main.rs` | 参数解析、单实例锁（`src/instance_lock.rs`） |
| `src/run.rs` | 看门狗重启循环、单轮服务编排、Xvfb/VNC 管理、IPC 协议 |
| `src/server.rs` | HTTP 总调度与 OpenAI/Anthropic/Admin 路由 |
| `src/webui.rs` | WebUI 静态资源、VNC WebSocket 代理、托管数据目录操作 |
| `src/config_patch.rs` | 配置写回校验（对齐 validator.js 文案）与 YAML patch |
| `src/metrics.rs` | 系统指标（CPU/内存/内核版本） |
| `src/typed.rs` | 配置类型化视图（serde 结构体，未知字段保序） |
| `src/queue.rs` | 队列、心跳、伪流式 |
| `src/parse.rs` | OpenAI 请求解析与虚拟上下文模板 |
| `src/bridge.rs` | 引擎桥客户端 |
| `src/config.rs` | 配置加载、默认值、校验 |
| `src/history.rs` | SQLite 历史记录 |
| `src/respond.rs` `src/errors.rs` | 响应、错误码表与 thiserror 错误类型 |
| `bridge/bridge.mjs` | Node 引擎桥 |
| `tests/integration.rs` | 端到端集成测试 |
