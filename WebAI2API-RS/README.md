# WebAI2API-RS

WebAI2API 的 Rust 重写。HTTP、队列、配置、历史、统计、日志、进程管理、
Worker 调度、模型目录、故障转移和 19 个站点流程由 Rust 承担。
Node 子进程只运行 Camoufox/Clearcote 浏览器 SDK 和通用页面、事件、下载 RPC。
原 Node 服务仍可独立运行，适合迁移期间对照。

## 架构

```
webai2api (Rust)
  ├─ 单实例锁（flock，进程死亡自动释放）、崩溃重启（退出码 78 不重启，其余上限 3 次）
  ├─ Linux 下按需拉起 Xvfb / x11vnc（scripts/start-xvfb.sh、start-vnc.sh），
  │  子进程退出有监控：Xvfb 退出触发整轮重启，x11vnc 退出只置 VNC 不可用
  ├─ axum：OpenAI / Anthropic / Admin API、SSE、WebUI 静态资源、VNC WebSocket 代理
  ├─ runtime.rs / scheduler.rs / catalog.rs：Worker、模型、调度与故障转移
  ├─ adapters/：19 个站点的导航、上传、响应解析和错误判断
  └─ Unix socket JSON 行协议 ↓
bridge/browser-runtime.mjs (Node)
  └─ bridge/engine/：浏览器 SDK 启动与生命周期；仅暴露通用浏览器操作
```

桥 spawn 时 setsid 自成进程组：停机时整组击杀， camoufox 等孙进程不会成为孤儿。
桥协议提供浏览器启动、页面批量操作、响应事件、路由、下载、Cookie 和关闭。
生产路径不调用 Node `backend.generate`、`PoolManager`、`Worker` 或站点适配器。
显式 `--bridge-script` 仅供旧版 mock 桥集成测试兼容。

停机路径：`/admin/stop`、IPC `STOP`、Ctrl-C、SIGTERM 都收敛到同一条清理通道
（桥 shutdown → 杀进程组 → 清单实例锁），不存在绕过浏览器清理的退出路径。

## 构建与运行

```bash
cd WebAI2API-RS
cargo build --release

# 在原仓库根目录安装浏览器依赖并编译 WebUI
cd ..
corepack pnpm install --frozen-lockfile
npm run init
npm ci --prefix webui --ignore-scripts
npm --prefix webui run build

# 回到 RS 目录运行；原 Node 服务可继续在另一端口作对照
cd WebAI2API-RS
./target/release/webai2api --src-root .. --data-dir ../data

# 登录模式、虚拟显示与原版参数一致
./target/release/webai2api --src-root .. -login=workerName
./target/release/webai2api --src-root .. -xvfb -vnc      # 仅 Linux

# 指定独立状态目录；配置、用户配置文件、日志、历史和临时文件都写入此处
./target/release/webai2api --src-root .. --data-dir /var/lib/webai2api
```

生成密钥：`./target/release/webai2api-genkey`，输出 `sk-` 加 48 位十六进制。

运行要求：Rust 1.88+、Node.js 20+，以及原仓库已安装的依赖
（`camoufox-js`、`clearcote`、`playwright-core` 等）。浏览器内核仍由
`npm run init` 下载到原仓库的 `camoufox/` 目录。

## 兼容性

- 默认读取原仓库的 `data/config.yaml`；指定 `--data-dir` 后从该目录读取配置，
  并将用户配置文件、日志、历史和临时文件都放在该目录。
- `data/history/history.db` 表结构不变，可直接打开原有数据库。
- WebUI 从 checkout 内 `webui/dist` 加载；`webui-dist` 是指向该目录的相对符号链接。
  可用 `WEBAI2API_WEBUI_DIR` 指定其他目录。重建时在仓库根目录执行
  `npm ci --prefix webui --ignore-scripts && npm --prefix webui run build`；Docker 构建会自行编译 WebUI。
- 保留原版的已知行为：非流式 429 文案里的队列上限显示为 `undefined`、
  `/v1/chat/completions` 前缀匹配、流式请求不受队列上限约束。
- 配置写回是合并式的：只更新提交的键，其余键保留。YAML 注释仍会在写回时丢失，
  与原版 `yaml.stringify` 的行为相同；写回是原子的（临时文件 + rename），
  加载器校验失败会回滚原文。

## 测试

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets -- --test-threads=1  # Rust 单测与 HTTP/进程集成测试
node --test bridge/browser-runtime.test.mjs   # 12 项通用桥协议测试
```

集成测试覆盖旧 mock 桥兼容与通用浏览器 RPC 到 Rust 适配器的 HTTP 生成链路，
以及安全模式、桥崩溃回收、并发配置、媒体清理、锁互斥和启动期间 SIGTERM。
CI 另在 Linux Docker 镜像中启动并关闭真实 Camoufox、检查 HTTP 与 WebUI。

CI：`.github/workflows/rust-ci.yml` 运行 `cargo fmt --check`、
`cargo clippy --all-targets -- -D warnings`、`cargo test`、release 构建、版本检查与 Docker 冒烟。

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
| `src/runtime.rs` `src/scheduler.rs` `src/catalog.rs` | Rust Worker、模型、调度与故障转移 |
| `src/adapters/` | 19 个 Rust 站点流程 |
| `src/browser_rpc.rs` | 通用浏览器 RPC 客户端 |
| `src/bridge.rs` | 旧 mock 桥测试兼容客户端 |
| `src/config.rs` | 配置加载、默认值、校验 |
| `src/history.rs` | SQLite 历史记录 |
| `src/respond.rs` `src/errors.rs` | 响应、错误码表与 thiserror 错误类型 |
| `bridge/browser-runtime.mjs` `bridge/engine/` | 通用 Node 浏览器 SDK 桥 |
| `tests/integration.rs` | 端到端集成测试 |
