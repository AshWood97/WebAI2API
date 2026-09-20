---
feature: camoufox-ff152-upgrade
status: delivered
updated: 2026-09-20
branch: feat/camoufox-ff152
commits: beac6c9..(see branch head)
---

# Camoufox FF152 升级

## Report

**What was built** — 将 WebAI2API 的 Camoufox 基座从 FF135.0.1-beta.24 / camoufox-js 0.8.3 / playwright-core 1.57 升级到 **FF152.0.4-beta.30 / camoufox-js 0.12.0 / playwright-core 1.60.0**。安装链路改用 `scripts/camoufoxRelease.js` 钉选版本；通过 `CAMOUFOX_INSTALL_DIR`（`src/backend/engine/camoufoxEnv.js`，必须先于 camoufox-js import）实现项目内便携浏览器目录，取消 pkgman/locale 补丁。仅保留 **SOCKS5 `proxyUrl.origin=null`** 补丁（`patches/camoufox-js@0.12.0.utils.patched.js`）。启动器不再硬编码 `ff_version:135`，持久化指纹 UA 自动迁移到已安装 Firefox 主版本；默认 `humanizeCursor:"camou"` 启用 152 内核拟人轨迹；暴露 `browser.camoufox.*`（mainWorldEval / enableCache / disableInstantAnimations / humanizeMaxTime / blockWebRtc / geoip）。`disableInstantAnimations` 在 FF152 properties 支持时写入 camoufox config。preflight 识别内核版本，major&lt;146 硬失败。

**Verification** —
- `node scripts/test-camoufox-upgrade.mjs` → **28/28 PASS**（含 FF152 properties 键检测）
- `npm run init` → 下载 `camoufox-152.0.4-beta.30-mac.arm64.zip`，`version.json` 为 `152.0.4-beta.30`
- preflight 日志 → `Camoufox 内核: 152.0.4-beta.30` / 自检通过
- supervisor 冒烟 → `Camoufox 152.0.4-beta.30 (Firefox 152)`，`内核拟人轨迹: 开`，Worker 就绪，`/admin/status` 返回 `3.8.0`，`/v1/models` 正常
- `fingerprint.json` UA → `Firefox/152.0`（已迁移）
- `node_modules/camoufox-js/dist/utils.js` 含 PATCHED socks5 `protocol//host`
- 评审 critical：`pnpm-workspace.yaml` 无效 `allowBuilds` 占位符 → 已改为 `onlyBuiltDependencies`；`disableInstantAnimations` 死开关 → 已接线；`init.js` 路径基准 → 对齐 `PROJECT_ROOT`

**Journey log** —
1. camoufox-js@0.12 在 `pkgman` **模块加载时**读取 `CAMOUFOX_INSTALL_DIR`，env 必须在更早的 ESM import 中设置。
2. 上游 0.12 仍未修 SOCKS5 `URL.origin=null`，便携路径改 env 后可去掉 pkgman/locale 补丁，但 utils 补丁仍要跟版本重打。
3. camoufox-js `window:[w,h]` 只在**未传 fingerprint** 时生效；本项目始终用持久化 fingerprint，尺寸 spoof 来自 `fromBrowserforge`。
4. FF152 `properties.json` 已含 `disableInstantAnimations` / `humanize` / `allowMainWorld`，可 properties 门控写入。
5. main 工作区未提交的 API 3.7.0（`/health` 等）不在本分支；`pnpm-workspace.yaml` 在 main 上也有同样的 allowBuilds 占位符污染，合并时需一并清理。

## [S1] Problem

WebAI2API 的反检测基座 Camoufox 停在 **浏览器 FF135.0.1-beta.24（2025-03）+ camoufox-js 0.8.3 + playwright-core 1.57**。上游已明确 FF135「不适合现代反爬」，并在 2026-09 发布 **v152.0.4-beta.30**（camoufox-js 对应 **0.12.0**）。

本地硬编码与补丁绑定旧栈：

- `scripts/init.js` 钉死下载 `135.0.1-beta.24` 与 `version.json`
- `src/backend/engine/launcher.js` 钉死 `ff_version: 135` 与 UA `TARGET_VERSION = "135.0"`
- `patches/camoufox-js@0.8.3.*` 针对 0.8.3 源码；0.12.0 上 SOCKS5 `proxyUrl.origin=null` **仍未修**，必须重打
- 持久化 `fingerprint.json` 仍可能写 FF135 UA，与新内核不一致会露馅
- 未启用 152 系新能力：WebRTC 代理 IP 防泄漏修复、humanize 轨迹修复、`CAMOUFOX_INSTALL_DIR` 便携安装、main_world_eval、窗口尺寸/viewport 协同（#666）

用户决策：worktree 隔离实现；完整升级 + 启用新能力；目标浏览器 **v152.0.4-beta.30**。

## [S2] Design

### 版本矩阵

| 组件 | 目标 |
|------|------|
| Camoufox browser | `v152.0.4-beta.30`（GitHub release tag） |
| camoufox-js | `^0.12.0` |
| playwright-core | `1.60.0`（peer `<1.61.0` 的最新稳定） |
| fingerprint-generator | `^2.1.86`（对齐 camoufox-js） |
| better-sqlite3（项目侧） | 保持 `^12.5.0`（历史库）；camoufox-js 自带嵌套 v13 |
| package version | `3.8.0` |

### 安装与路径

- `scripts/init.js`：下载 URL 与 `version.json` 改为 `{ version: "152.0.4", release: "beta.30" }`；资源名 `camoufox-152.0.4-beta.30-{win|mac|lin}.{x86_64|arm64}.zip`
- 新增 `src/backend/engine/camoufoxEnv.js`：在 **任何** `camoufox-js` import 之前设置 `process.env.CAMOUFOX_INSTALL_DIR ||= <cwd>/camoufox`
- 0.12 `pkgman.js` 原生读该 env，**不再维护 locale/pkgman 便携补丁**
- `launcher.js`、`scripts/init.js`、`preflight.js` 顶部 side-effect import 或显式设置该 env
- Docker：容器 cwd 为 `/app` 时同逻辑指向 `/app/camoufox`（与既有 `browser.path` 修正一致）

### 补丁

- 仅保留 SOCKS5 补丁，基于 0.12.0 `dist/utils.js` 生成 `patches/camoufox-js@0.12.0.utils.patched.js`
- 修改点：`server: proxyUrl.origin` → `server: proxyUrl.protocol + '//' + proxyUrl.host`
- `scripts/postinstall.js` 的 `CAMOUFOX_PATCHES` 映射改为仅 utils@0.12.0
- 删除对 locale/pkgman@0.8.3 补丁的依赖（preflight 不再要求）

### Launcher 契约

启动选项：

- **不传** `ff_version`，由 camoufox-js 读取 `version.json` 得到已安装主版本（避免 spoof 警告与硬编码漂移）；如配置显式 `browser.ffVersion` 则透传并保留 `i_know_what_im_doing`
- `block_webrtc: true`（默认保持）+ `geoip: true`（代理场景防泄漏；152 修复 WebRTC IP leak）
- `humanize`：当 `browser.humanizeCursor === 'camou'` 时传 `true` 或 `humanize:maxTime` 秒数；152 beta.28+ 修复了 juggler 轨迹
- `main_world_eval`：配置 `browser.camoufox.mainWorldEval`（默认 false）→ `allowMainWorld`
- 可选 `browser.camoufox.enableCache` → `enable_cache`
- 可选 `browser.camoufox.disableInstantAnimations`：写入 `firefox_user_prefs['ui.prefersReducedMotion']=1`（已有）并在 config 尝试非标准键时忽略未知属性——**仅**在 properties.json 含对应键时写入 camoufox config（152 可能支持；135 无此键）
- 窗口尺寸：指纹 `screen.*` 通过 `fromBrowserforge` 决定 spoof；camoufox-js 的 `window:[w,h]` **仅在未传 fingerprint 时生效**，本项目始终传持久化 fingerprint，因此 window 仅作日志/未来扩展，不作为主要契约
- `disableInstantAnimations`：当 `browser.camoufox.disableInstantAnimations=true` 且 FF152 `properties.json` 含 `disableInstantAnimations` 时写入 camoufox config；同时保持 `ui.prefersReducedMotion=1`
- 不强制 `page.setViewportSize`（避免与 #666 spoof 维度冲突）

指纹持久化：

- 读取 `data/camoufoxUserData*/fingerprint.json`
- 若 `navigator.userAgent` 中 Firefox/`rv` 主版本 ≠ 当前安装主版本，则重写为 `{major}.0` 并保存
- WebGL 配置校验逻辑保留；失效则重新 `sampleWebGL`

### 配置面（config.example.yaml + config 默认值）

```yaml
browser:
  humanizeCursor: "camou"   # 默认改为 camou（推荐，用内核拟人轨迹）
  # ffVersion 省略 = 跟随已安装 Camoufox
  camoufox:
    mainWorldEval: false
    enableCache: false
    disableInstantAnimations: false
    humanizeMaxTime: 1.5    # 秒；仅 camou 模式生效
```

`src/config/index.js` 补默认值；`manager.js` WebUI 持久化字段可选透传（无 WebUI 字段时用 yaml）。

### 运维

- `preflight.js`：side-effect import `camoufoxEnv.js`；检查 `camoufox/version.json`；解析 major；**不**检查 0.8.3 补丁；检查 utils@0.12 补丁 MD5（若补丁文件存在）；major &lt; 146 **硬失败**（强制 npm run init，避免继续用过旧内核）
- `runtime/status` 可后续暴露 camoufox 版本（可选，不阻塞）
- CHANGELOG 记录 3.8.0

### 验证边界

- 单元：UA 迁移、init URL/version.json 构造、补丁 MD5 映射、launch option 组装（mock camoufox-js 可选；优先纯函数抽出）
- 冒烟：`npm run init` 下载 152；preflight 通过；`node supervisor.js` 启动；`/health` `/ready`；日志出现 FF152 / 浏览器已启动
- 不在此 PR 内验证各 AI 站点适配器 DOM 全量回归（需真实账号）

## [S3] Out of Scope

- 不合并 main 上未提交的 3.7.0 协议升级（本 worktree 基于 beac6c9）
- 不实现 gpt4free fallback / Anthropic 协议（属上次升级范围）
- 不自动登录任何 AI 站点
- 不保证全部适配器在 FF152 下选择器仍然有效（需后续按站点回归）
- 不升级 Node 引擎要求以外的无关依赖大版本（除上述矩阵）

## Tasks

- [x] T1: 写入并维护本 Spec — acceptance: docs/compose/spec/camoufox-ff152-upgrade.md 存在且 anchors 稳定 (covers: S2)
- [x] T2: package.json 依赖与版本 3.8.0 + pnpm install — acceptance: worktree 中 camoufox-js 0.12.x、playwright-core 1.60.0 (covers: S2; depends: T1)
- [x] T3: 生成 0.12 SOCKS5 utils 补丁并更新 postinstall/preflight 映射 — acceptance: 补丁文件存在，postinstall 只映射 utils@0.12.0，MD5 与 node_modules 一致 (covers: S2; depends: T2)
- [x] T4: camoufoxEnv.js + init.js 152 下载/version.json — acceptance: URL 与 version.json 指向 152.0.4-beta.30；env 在 import camoufox-js 前生效 (covers: S2; depends: T2)
- [x] T5: launcher 指纹迁移 + 启动选项 + 视口策略 — acceptance: UA 跟随安装版本；不强制 setViewportSize；humanize/mainWorldEval/enableCache 可配置 (covers: S2; depends: T3,T4)
- [x] T6: config 默认值与 example 注释 — acceptance: humanizeCursor 默认 camou；camoufox 子配置有默认值 (covers: S2; depends: T5)
- [x] T7: 单元测试 + init/preflight/启动冒烟 — acceptance: 测试通过；服务 /admin/status 3.8.0 且浏览器 FF152 启动 (covers: S2; depends: T5,T6)
- [x] T8: 评审 critical 清零并 Finalize Spec — acceptance: status=delivered，Report 填写 (covers: S1,S2)
