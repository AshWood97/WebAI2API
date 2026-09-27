---
feature: dual-browser-engine-clearcote
status: plan
updated: 2026-09-21
base_sha: 7eade9b4876c8dbb0402e6b51eff8df3fb7506a9
executor: third-party harness implementation
recommended_backend: claude-execution (Claude Code, model alias sonnet, high effort)
---

# WebAI2API 双浏览器基座：Camoufox + Clearcote 执行计划

## 1. 目标与结论

在不改动现有适配器协议和 API 模型 ID 的前提下，为 WebAI2API 增加第二条可选浏览器基座：

- `camoufox`：保持当前默认和现有行为；
- `clearcote`：通过官方 Node SDK 的 Playwright `launchPersistentContext()` 接入 Chromium；
- `browser.engine` 可切换全局默认基座；`backend.pool.instances[].engine` 可单实例覆盖，因此同一次服务运行中可以同时启用两种基座；
- 切换在重启后生效，不做运行中的热切换；模型和适配器代码不感知基座。

结论是“可以做”，但不能把 Clearcote 当成另一个 Camoufox 压缩包：当前官方 Node SDK/发行版明确支持 Windows x64、Linux x64，macOS 仍在 roadmap；当前仓库在 macOS 上可以继续使用 Camoufox，但不能宣称 Clearcote 在 macOS 可用。Clearcote 的开源引擎/补丁和公开构建路线应作为本项目的免费、可验证路径，不引入 PRO 许可证或私有构建依赖。

## 2. 产品锁定与非目标

### 必须满足

1. 省略新配置时，现有 Camoufox 配置、数据目录、登录态、代理、WebUI 和启动命令保持兼容。
2. 同一实例的多个 Worker 继续共享一个持久化浏览器上下文；不同实例可以分别选择 Camoufox 或 Clearcote。
3. 两种基座的 profile 绝不混用。现有 `data/camoufoxUserData*` 路径保留；Clearcote 使用 `data/clearcoteUserData*`。
4. Clearcote 使用 SDK 的校验/缓存能力，或用户显式提供的可执行文件；禁止自行下载未校验的随机二进制。
5. 预检、运行时状态、数据目录管理、Docker/初始化说明和 WebUI 都能表达当前使用的基座。
6. 第三方 harness 必须在独立 worktree 执行；本计划为单一实现 writer，避免多个 writer 修改同一启动链。

### 明确不做

- 不在 WebAI2API 仓库内编译 Chromium；Clearcote 源码构建需要 Linux、约 120GB 磁盘和长时间构建，属于独立供应链流程。
- 不把 PRO/付费 Clearcote 功能、许可证密钥、私有二进制或自动购买接入项目。
- 不重写 `src/backend/adapter/`，不改变 OpenAI/Anthropic API、模型 ID、队列和故障转移语义。
- 不承诺所有 AI 网站在两种基座上都通过真实账号回归；真实站点回归列为可选人工验收。
- 不在本任务中修改 Astra 编排配置、启动 harness、提交或推送代码。

## 3. 现状证据与设计约束

- `src/backend/engine/launcher.js:initBrowserBase()` 目前直接调用 `Camoufox()`，并在启动前生成 Firefox 指纹、WebGL、UA 迁移和 Firefox prefs。
- `src/backend/pool/Worker.js` 只消费 `context/page`，并通过 `newPage()`、`page.goto()`、Cookie、文件上传等标准 Playwright 能力工作；这是两种基座的稳定接口边界。
- `src/backend/pool/PoolManager.js:initAll()` 按 `userDataDir` 共享浏览器，接入引擎后共享键必须至少包含 `engine + userDataDir`。
- `src/config/index.js` 的 `resolveUserDataDir()`、`flattenInstancesToWorkers()` 负责路径和实例展开；`src/config/manager.js`、`src/config/validator.js`、`webui/src/components/settings/{browser,workers}.vue` 负责配置面。
- `src/server/preflight.js` 目前无条件检查 Camoufox 可执行文件、版本、GeoLite 和补丁；改造后只能检查实际被配置引用的引擎。
- `src/utils/systemInfo.js` 目前只允许管理 `camoufoxUserData*`；双引擎后必须安全地识别两种前缀。

## 4. 锁定的配置契约

保持旧字段可读，并新增以下最小结构；实现时必须在 `config.example.yaml` 中给出完整注释：

```yaml
browser:
  engine: camoufox                 # camoufox | clearcote，全局默认
  path: ""                        # 旧字段，继续作为 Camoufox 可执行文件路径
  headless: false
  # 现有 fission/humanizeCursor/cssInject/proxy/camoufox 配置保留
  clearcote:
    path: ""                      # 空值=SDK 解析并校验缓存；非空=显式可执行文件
    platform: auto                 # auto | windows | linux；auto 按宿主机解析
    brand: Chrome
    fingerprintProfile: ""        # 可选，文件路径；空值使用每个 profile 的持久 seed
    timezone: ""
    acceptLanguage: ""
    geoip: true
    humanize: true
    webrtcIp: ""
    args: []                       # 仅 Clearcote/Chromium 参数，不自动继承 Firefox 参数

backend:
  pool:
    instances:
      - name: browser_default
        engine: camoufox            # 可省略，继承 browser.engine
        userDataMark: ""
        workers: [...]
      - name: browser_chromium
        engine: clearcote
        userDataMark: clearcote-main
        workers: [...]
```

规则：

- 只允许小写 `camoufox` / `clearcote`；未知值在配置校验阶段拒绝。
- `engine` 放在 instance，不放在单个 adapter worker；同一 instance 下的 Worker 必须共享同一基座。
- Camoufox 旧路径和旧 profile 不迁移、不重命名；Clearcote 自动使用独立目录。
- Clearcote 未显式提供 seed 时，在其 profile 目录写入项目侧身份元数据（例如 `.webai2api-clearcote.json`），首次生成随机高熵 seed，后续启动复用；不得用 instance 名称直接作为身份 seed。
- Clearcote 的 `humanize: true` 使用引擎原生 humanize；不要再叠加 ghost-cursor。只有用户明确选择旧的 `humanizeCursor: true` 且关闭 Clearcote 原生 humanize 时，才允许走现有 ghost-cursor 路径。
- Firefox 专属 `ffVersion`、`camoufox.*`、`firefox_user_prefs`、`webgl_config`、Camoufox canvas 配置不能透传给 Clearcote；反之亦然。

## 5. 执行任务与验证门

### T0 — 外部事实与依赖门（写代码前）

在 worktree 中确认当前 npm 上可用的 `clearcote` 版本、`launchPersistentContext` 导出、Playwright peer 兼容性、Windows/Linux x64 发行版和 SDK 校验行为。计划日期的官方 Node 文档列出 SDK `0.28.0`，但执行时必须重新核验并把实际选择的版本精确 pin 到 `package.json`/`pnpm-lock.yaml`，不得使用未经核验的 `^` 版本。

门：若 SDK 没有持久化上下文 API、无法和当前 Playwright 页面契约共存、或当前平台没有可验证的公开构建，立即写 `BLOCKED` 报告，不实现“猜测版”适配。

### T1 — 引擎契约与启动分派

涉及起点：`src/backend/engine/launcher.js`、`src/backend/engine/camoufoxEnv.js`、`src/backend/engine/camoufoxMeta.js`，新增 Clearcote 元数据/启动辅助模块时保持纯函数可测。

要求：

- 保留 Camoufox 启动链和 import 顺序；只在 `engine === clearcote` 时 lazy-import Clearcote SDK。
- 将 `initBrowserBase()` 变为引擎分派器，统一返回 `{ context, page, engine, runtime }`，保留旧调用方可用的 `context/page`。
- Clearcote 使用 `launchPersistentContext(userDataDir, options)`；选项只包含 `headless`、显式 `executablePath`、持久 seed/profile、platform/brand、proxy、geoip、timezone、language、humanize、webrtc 和显式 args。
- 用 `Set` 或等价结构跟踪全部活动 context，退出时关闭两种引擎的所有 context；不能继续只保存最后一个 context。
- 为每个 engine 提供可诊断的 `runtime` 元数据，至少包含 engine、版本/发行标识、宿主平台、binary source 和能力状态。

门：纯函数测试能证明引擎解析、选项隔离、profile seed 持久化、清理行为；Camoufox 旧测试不回归。

### T2 — 配置、profile、代理和 Worker/Pool 接线

涉及：`src/config/index.js`、`src/config/manager.js`、`src/config/validator.js`、`src/backend/pool/Worker.js`、`src/backend/pool/PoolManager.js`、`src/utils/proxy.js`。

要求：

- 实现全局默认 + instance override；展开后的 worker 带解析后的 engine。
- `resolveUserDataDir(mark, engine)` 保留旧 Camoufox 路径，并为 Clearcote 生成独立前缀；Pool 共享键加入 engine。
- Clearcote 走标准 Playwright proxy 对象。HTTP 代理要正确拆出 `server/username/password`；SOCKS5 无认证可直接传递，带认证不得静默丢密码：要么复用/扩展现有 relay 生命周期，要么明确拒绝并给出可操作错误。
- 对 context/page 的 humanize、close、re-init、共享 Worker 路径全部做同样的 engine-aware 处理。

门：配置校验拒绝未知 engine 和非法 Clearcote 字段；两个 instance 可在同一 Pool 中解析为不同 engine，且不会共享 profile 或 proxy 生命周期。

### T3 — 预检、初始化、依赖和容器

涉及：`src/server/preflight.js`、`scripts/init.js`、`package.json`、`pnpm-lock.yaml`、`Dockerfile`、`docker-compose.yaml`。

要求：

- 预检先得到配置引用的 engine 集合：仅 Camoufox 时不要求 Clearcote；仅 Clearcote 时不要求 Camoufox；两者都配置时两套都检查。
- Camoufox 原有补丁、版本、GeoLite 检查保持不变。
- Clearcote 预检检查平台、SDK 可导入、显式 path 存在性；不要在服务预检中无提示下载大文件。提供可选的 `init`/custom 预取入口时必须使用 SDK 的 checksum/GPG 校验链。
- Docker 补齐 Clearcote 官方 Node 文档所需的运行库和 `xz-utils`；不要全局给 Camoufox 追加 Chromium 参数。`--no-sandbox` 只能作为显式 Clearcote 参数/容器说明，不能偷偷降低原有安全边界。
- 不支持的 macOS Clearcote 必须给出明确错误和替代建议（继续用 Camoufox 或在官方支持后升级），不能 fallback 到普通 Chromium 后仍报告 `clearcote`。

门：选择单一引擎时不会被另一引擎缺失阻断；Docker/源码安装说明能解释二进制缓存、平台和 sandbox 前置条件。

### T4 — 运行时状态、数据管理和 WebUI

涉及：`src/server/api/openai/routes.js`、`src/server/api/openai/openapi.js`、`src/utils/systemInfo.js`、`webui/src/components/settings/browser.vue`、`webui/src/components/settings/workers.vue`、`webui/src/components/dash.vue`、必要时 `webui/src/stores/settings.js`。

要求：

- 保留现有 `camoufox` runtime 字段兼容性；新增通用 browser/engines/workers 状态，不能泄露代理密码或绝对路径中的敏感信息。
- 浏览器设置页增加全局 engine 选择和 Clearcote 专属字段；实例编辑页增加“继承/ Camoufox / Clearcote”选择，并展示 profile 隔离提示。
- 数据目录列表/删除只允许明确的 `camoufoxUserData*`、`clearcoteUserData*` 前缀，并继续阻止删除正在使用的目录。
- 保存配置后明确提示必须重启；Dashboard 显示实际运行的 engine，而不是把所有浏览器都写成 Camoufox。

门：`vite build` 通过；WebUI GET/POST 返回的新旧字段不丢失；运行时 JSON 在安全模式/未初始化时仍可序列化。

### T5 — 文档与测试交付

涉及：`config.example.yaml`、`README.md`、`README_EN.md`、`CHANGELOG.md`，新增针对引擎契约的脚本测试，必要时更新 `docs/compose/spec/`。

必须覆盖：

1. 全局切换示例；
2. Camoufox + Clearcote 同时运行的双 instance 示例；
3. profile 不可跨引擎复用；
4. Clearcote 当前 Windows/Linux x64 限制和 macOS 状态；
5. Clearcote 是开源/可验证路线，但不保证绕过任何网站风控，使用者需遵守站点条款；
6. 依赖的精确版本、SDK 自动校验/缓存、可选预取和 Docker sandbox 注意事项。

## 6. 统一验收清单

实现完成后，第三方 harness 必须在 `report.md` 中记录实际执行命令、退出码和原始摘要；至少运行：

```bash
pnpm install --frozen-lockfile
node --check src/backend/engine/launcher.js
node --check src/backend/pool/Worker.js
node --check src/backend/pool/PoolManager.js
node scripts/test-browser-engines.mjs       # 若采用其他文件名，在报告中说明
node scripts/test-camoufox-upgrade.mjs
node scripts/test-upgrade.mjs
node scripts/test-ops-docs.mjs
pnpm --dir webui build
```

在有受支持的 Clearcote 二进制和运行库的 Windows/Linux x64 环境，再运行无账号浏览器冒烟：

- Camoufox profile 启动、`about:blank`、Cookie 写入/关闭/重启读取；
- Clearcote profile 启动、`about:blank`、Cookie 写入/关闭/重启读取；
- 同一进程先后或同时启动两个 engine，确认两者 context/page 都是标准 Playwright 对象；
- 验证 `navigator.webdriver`、UA/UA-CH、WebGL/Canvas 检查只作为观测证据，不把“通过某个检测站”当作项目保证；
- 验证代理成功/失败路径，尤其是 SOCKS5 认证不能静默降级；
- 用现有 `test` adapter 做最小导航/截图/文件上传回归；真实 AI adapter 只在有账号时做人工 smoke。

若执行环境是当前 macOS ARM64，Camoufox 可做本地冒烟；Clearcote 只能完成配置/分派/不支持平台错误测试，必须在报告中标记 `UNVERIFIED`，不能把它写成 Clearcote 通过。

## 7. 交付、风险与停止条件

### 交付物

- 代码、配置、依赖锁文件、Docker/初始化支持、WebUI、文档和自动化测试；
- `report.md`：首行 `VERDICT`，列出修改文件、命令/退出码、Camoufox 与 Clearcote 各自的实际证据、未验证项和残余风险；
- 不要求提交 commit；若 harness 默认会提交，必须只在用户另行授权后执行。

### 主要风险

- Clearcote 的 Chromium 版本、Node SDK peer 版本、平台矩阵会随上游变化；T0 的官方核验结果优先于本计划中的示例版本。
- Camoufox 与 Clearcote 的 UA、WebGL、viewport、WebRTC、代理认证语义不同；任何“共用 Camoufox 指纹文件/启动参数”的实现都应视为缺陷。
- Clearcote 当前 macOS 不可用；不能为了让本机绿灯而 fallback 到普通 Chromium。
- Clearcote SDK 的自动下载可能增加首次启动延迟；生产部署应使用固定版本、固定缓存并在部署阶段预取/校验。

### 必须停止并报告 BLOCKED 的情况

- 无法确认官方 SDK 的持久化 API或 checksum 供应链；
- 只能通过普通 Chromium 或未校验二进制实现 Clearcote 名义支持；
- 需要破坏现有 Camoufox profile、改变 adapter API 或静默丢失代理认证才能接入；
- 受支持平台上的双引擎最小启动无法复现，且没有明确的上游/运行库证据。

## 8. 第三方 harness 执行约束

- 这是单一 `implement` job；先读本文件和现有源码，再按 T0→T5 执行，不要另起一个产品设计任务。
- worker brief 第一行必须是 `YOU ARE THE WORKER. DO NOT SPAWN`；不得加载编排/嵌套 harness，不得调用 spawn 脚本，不得询问使用哪个 harness。
- 推荐路由为 `claude-execution` / Claude Code `sonnet` / `high`；当前用户配置把 `implement` 默认映射为 `self`，真正交给外部 harness 时必须由父级显式覆盖，不得悄悄改成 Codex 自己实现。
- 实现 worker 只在独立 worktree 写应用代码；父级 Codex 负责审查实际 diff、重新运行关键测试，并判断 `UNVERIFIED`，不能只采信 worker 的完成文案。

## 9. 资料依据（2026-09-21 核验）

- [Clearcote 官方 Playwright 文档](https://www.clearcotelabs.com/docs/playwright)：Node SDK、Playwright drop-in、persistent context、代理和校验缓存说明。
- [Clearcote Node SDK README](https://github.com/clearcotelabs/clearcote-browser/blob/main/sdk/node/README.md)：当前 API、`launchPersistentContext`、平台说明、`fingerprint`/`fingerprintProfile` 和发行版校验顺序。
- [Clearcote 官方仓库](https://github.com/clearcotelabs/clearcote-browser)：开源仓库、构建方式、BSD-3-Clause 声明及 roadmap。
- [Clearcote BUILDING.md](https://github.com/clearcotelabs/clearcote-browser/blob/main/docs/BUILDING.md)：从源码构建的资源/时间成本，支持“不在 WebAI2API 内编译”的范围判断。

