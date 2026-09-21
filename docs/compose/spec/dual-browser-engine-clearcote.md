---
feature: dual-browser-engine-clearcote
status: delivered
updated: 2026-09-21
branch: feat/dual-browser-engine-clearcote
commits: 7eade9b4876c8dbb0402e6b51eff8df3fb7506a9..working-tree-uncommitted
---

# WebAI2API 双浏览器基座：Camoufox + Clearcote

## Report

**What was built** — 3.10.0 双浏览器基座：`browser.engine`（默认 `camoufox`）与 `backend.pool.instances[].engine` 可覆盖；同一进程可同时运行 Camoufox 与 Clearcote。Clearcote 经官方 npm `clearcote@0.30.0` 的 `launchPersistentContext()` 懒加载接入，profile 使用 `data/clearcoteUserData*`，Pool 共享键为 `engine+userDataDir`。profile 内 `.webai2api-clearcote.json` 持久高熵 seed；Firefox 专属字段不透传 Clearcote。预检只检查配置引用的引擎；macOS 选 Clearcote 明确报错，不回退普通 Chromium。SOCKS5 带认证走 proxy-chain relay（不静默丢密码），并支持多 relay 清理。WebUI/文档/测试/Docker 运行库已同步。

**Verification** —
- `pnpm install --frozen-lockfile` → PASS
- `node --check` launcher/Worker/PoolManager/clearcoteMeta/engineContract/proxy → PASS
- `node scripts/test-browser-engines.mjs` → **77/77 PASS**（含 linux Clearcote 预检 SDK 解析、嵌套 runtime 脱敏、macOS 禁止 Chromium 回退文案）
- `node scripts/test-camoufox-upgrade.mjs` → 27/27 PASS
- `node scripts/test-upgrade.mjs` → 29/29 PASS
- `node scripts/test-ops-docs.mjs` → 12/12 PASS
- `pnpm --dir webui build` → PASS（dist 含 clearcote/引擎 UI 字符串）
- 预检抽样：`engine=clearcote` @ linux → `[]`（SDK 可解析）；@ darwin → 明确 macOS 错误；仅 Clearcote 时不检查 Camoufox 可执行文件/GeoLite
- **UNVERIFIED**：macOS ARM64 上 Clearcote 真实启动/二进制冒烟（官方不支持 macOS）；Windows/Linux x64 上的无账号浏览器冒烟需在受支持环境执行

**Journey log** —
1. 计划示例 SDK 0.28.0 已过时；执行时 npm 最新为 **0.30.0**，已精确 pin。
2. clearcote 为 ESM-only：`require.resolve('clearcote/package.json')` 必然失败；预检必须走 `node_modules/clearcote/package.json` 磁盘路径（评审 critical，已修；复评 PASS，无新 critical）。
3. `sanitizeRuntimeForApi` 必须递归处理 `workers[].runtime`，否则嵌套绝对路径会泄漏。
4. pnpm 11 默认拦截 esbuild/core-js 构建脚本；`pnpm --dir webui build` 需允许构建（`webui/.npmrc` + workspace allowBuilds）。
5. SOCKS5 认证不能丢给 Chromium：复用 proxy-chain relay，并改为 Set 跟踪多个匿名代理生命周期。

## [S1] Problem

WebAI2API 当前只有 Camoufox 一条浏览器基座。上游 Clearcote 提供开源、可 SHA-256 校验的 Chromium 构建与官方 Node SDK（`launchPersistentContext`），可在 Windows/Linux x64 上作为第二基座。需要在不改动适配器协议与 API 模型 ID 的前提下增加可选 Clearcote，并与现有 Camoufox 配置/数据目录/启动链兼容。

macOS 尚未获官方 Clearcote 发行版支持；本机可继续用 Camoufox，但不得宣称 Clearcote 在 macOS 可用，也不得 fallback 到普通 Chromium 后仍报告 `clearcote`。

## [S2] Design

### T0 外部事实（2026-09-21 核验）

| 项 | 事实 |
|----|------|
| npm 包 | `clearcote@0.30.0`（精确 pin，不用 `^`） |
| 持久化 API | `launchPersistentContext(userDataDir, options?) → Promise<BrowserContext>` |
| 普通启动 | `launch(options?) → Promise<Browser>`（Playwright drop-in） |
| peer | 包依赖 `playwright-core@^1.49.0`；项目锁定 `playwright-core@1.60.0` 兼容 |
| 平台 | 官方支持 Windows x64、Linux x64；macOS 仍在 roadmap |
| 二进制 | executablePath > CLEARCOTE_BINARY > 下载固定 release 并 SHA-256 校验；免费 GitHub 构建，不引入 PRO 许可证 |
| Node | engines `>=20`（项目要求 v20+） |
| Linux 运行库 | 需 `xz-utils` 及 Chromium 运行库；容器可显式 `--no-sandbox` |
| SOCKS5 认证 | Chromium/SDK 无法原生认证——项目侧走 proxy-chain relay 或明确拒绝，不得静默降级 |
| 免费并发 | 无 license 时官方免费档默认同时 1 个 Clearcote 浏览器；文档已说明 |

### 配置契约

```yaml
browser:
  engine: camoufox                 # camoufox | clearcote
  path: ""                        # 旧字段，Camoufox 可执行文件路径
  headless: false
  clearcote:
    path: ""                      # 空=SDK 解析校验缓存；非空=显式 executablePath
    platform: auto                 # auto | windows | linux
    brand: Chrome
    fingerprintProfile: ""
    timezone: ""
    acceptLanguage: ""
    geoip: true
    humanize: true
    webrtcIp: ""
    args: []

backend:
  pool:
    instances:
      - name: browser_default
        engine: camoufox            # 可省略，继承 browser.engine
      - name: browser_chromium
        engine: clearcote
        userDataMark: clearcote-main
```

规则：

- 只允许小写 `camoufox` / `clearcote`；未知值配置校验拒绝。
- `engine` 在 instance 级；同一 instance 的 Worker 共享基座。
- Camoufox：`data/camoufoxUserData*`；Clearcote：`data/clearcoteUserData*`。
- Clearcote seed：profile 内 `.webai2api-clearcote.json`，高熵随机，禁止用 instance 名。
- Clearcote `humanize: true` 用引擎原生；仅 `humanizeCursor===true` 且 Clearcote humanize=false 时 ghost-cursor。
- Firefox 专属字段不得传给 Clearcote；反之亦然。
- 切换重启后生效；适配器/模型 ID 不感知基座。
- Pool 共享键 = `engine + userDataDir`。
- 预检只检查引用到的引擎；macOS Clearcote → 明确错误。

### 引擎启动契约

`initBrowserBase(config, options)` 分派返回 `{ context, page, engine, runtime }`。

- 全部活动 context 用 `Set` 跟踪；cleanup 关闭所有 context。
- Clearcote 仅 `engine === 'clearcote'` 时 lazy-import。
- Camoufox 启动链与 import 顺序保持。

### 代理

- Clearcote：Playwright `{ server, username?, password? }`。
- SOCKS5 无认证：`socks5://host:port`。
- SOCKS5 带认证：proxy-chain relay（`http://127.0.0.1:port`），多实例 Set 清理。
- HTTP：正确拆分 server/username/password。

### 运行时 / WebUI / 数据目录

- runtime 保留 `camoufox`；新增 `browser.{defaultEngine,engines,workers}`（递归 basename 脱敏）。
- 数据管理仅 `camoufoxUserData*` / `clearcoteUserData*`。
- WebUI：全局 engine + Clearcote 字段；instance 继承/覆盖；保存提示重启。

## [S3] Out of Scope

- 在仓库内编译 Chromium；PRO 许可证/私有构建。
- 重写 adapter 层或改 API 模型 ID/队列语义。
- 运行中热切换引擎。
- 真实 AI 站点账号双引擎回归。
- 默认 git commit/push（计划要求用户另行授权）。
- 宣称 Clearcote 绕过任何网站风控。

## Tasks

- [x] T1: 引擎契约与启动分派 — acceptance: `initBrowserBase` 按 engine 分派；返回 `{context,page,engine,runtime}`；Clearcote lazy-import；context Set 清理；纯函数测试通过；Camoufox 旧测试不回归 (covers: S2)
- [x] T2: 配置/profile/代理/Pool 接线 — acceptance: 全局+instance engine；`resolveUserDataDir` 双前缀；Pool 键含 engine；未知 engine 校验拒绝；SOCKS5 认证走 relay；两 instance 可解析不同 engine (covers: S2; depends: T1)
- [x] T3: 预检/初始化/依赖/容器 — acceptance: 预检按引用引擎集合；macOS Clearcote 明确错误；`clearcote@0.30.0` 精确 pin；Docker 说明 xz-utils/运行库/sandbox (covers: S2; depends: T2)
- [x] T4: 运行时状态/数据管理/WebUI — acceptance: runtime 含双引擎字段且不泄密；WebUI 可配 engine/clearcote；数据目录仅两前缀；vite build 通过 (covers: S2; depends: T2)
- [x] T5: 文档与测试交付 — acceptance: config.example/README/CHANGELOG 覆盖双引擎、平台限制、profile 隔离、免费路径；`test-browser-engines.mjs` 通过；验收清单命令结果记入 Report (covers: S2; depends: T1-T4)
