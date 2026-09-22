---
feature: dual-browser-engine-clearcote-followup-hardening
status: in-progress
updated: 2026-09-22
branch: feat/dual-browser-engine-clearcote
commits: # filled at delivery
---

# Clearcote 双浏览器基座：执行后加固与验收

## Report

## [S1] Problem

Camoufox + Clearcote 双引擎基座已在 `feat/dual-browser-engine-clearcote` 落地（见 `dual-browser-engine-clearcote.md`），静态测试通过，但独立复核仍有 P1 安全与生命周期缺口，且真实 Clearcote 启动未验证：

1. `userDataMark` / `isManagedUserDataFolder()` 用宽松 `startsWith`，存在路径穿越、伪造前缀（如 `clearcoteUserDataEvil`）和跨引擎目录删除风险。
2. `buildClearcoteLaunchOptions` 同时写入 `fingerprint`（自动 seed）与 `fingerprintProfile`，身份语义不清晰。
3. sandbox 策略未在应用层显式表达；免费/PRO 状态硬编码 `proLicense: false`，SDK 会自动发现 `CLEARCOTE_LICENSE_KEY` 与 `~/.clearcote/license.key`。
4. 主动 `cleanup()` 期间 Worker 的 `close` handler 可能 `_reinit()` 重新拉起 context；`_reinit()` 无 single-flight。
5. seed 文件损坏会静默覆盖为新身份；WebUI args 按逗号切分，无法 round-trip `--disable-features=A,B`。
6. runtime 把 SDK package 版本当成浏览器版本；`report.md` 仍写 “working tree (uncommitted)”，与已提交 head 矛盾。

本加固不改变 OpenAI 兼容 API、模型 ID、适配器协议，也不改变 Camoufox 默认行为。

## [S2] Design

### T0 外部事实（2026-09-22 核验 clearcote@0.30.0）

| 项 | 事实 | 对本加固的含义 |
|----|------|----------------|
| pin | `clearcote@0.30.0`（lockfile 一致） | 默认不改 pin |
| 浏览器 release | `RELEASE.version = 149.0.7827.114`，`RELEASE.tag = v0.1.0-pre.22` | runtime 浏览器版本用 `RELEASE`/`browser.version()`，不得用 SDK 0.30.0 |
| fingerprintProfile | 文档：与 `fingerprint` seed **可同时**传；profile 字段覆盖 seed，缺省字段回退 seed | 计划要求互斥。采用计划：配置了 `fingerprintProfile` 时 **不传** 自动 seed（SDK 允许省略 `fingerprint`）；无 profile 时才用持久 seed。兼容且语义单一 |
| SOCKS5 认证 | README 明确：Chromium 无法原生 SOCKS5 认证，凭据会被丢弃；需本地 relay。`toProxySpec` 只做 `{server,username,password}` 拆分 | **不能**去掉 relay；强化 context 绑定生命周期 |
| license | `resolveLicenseKey` / `licenseKeySource` 会读 option → `CLEARCOTE_LICENSE_KEY` → `~/.clearcote/license.key` | 免费模式启动前用 `licenseKeySource()` 检测；非 `none` 则拒绝继续（除非显式允许检测到的 license），不读不打印 key |
| sandbox | 容器/root 可能需 `--no-sandbox` 或 setuid chrome-sandbox；`serveNeedsNoSandbox(linux, uid0)` | 默认安全 sandbox；关闭必须显式配置并写入 runtime/文档 |
| 平台 | Windows x64 / Linux x64；macOS roadmap | preflight 拒绝 darwin；P3 真实 E2E 仅在 x64 宿主机 |

### [S2] Profile 隔离与目录安全（P1-A）

- 新增 `validateUserDataMark(mark)`：允许空（默认目录）或单层标识符 `^[A-Za-z0-9_-]+$`；拒绝空串以外的空白、路径分隔符、`..`、绝对路径、控制字符。
- `resolveUserDataDirForEngine()`：先校验 mark，再拼 `data/<prefix>[_mark]`，对 `path.resolve` 结果做前缀边界检查（必须位于 `data/` 内且不等于 `data/` 本身以外的越界路径）。
- 同名 mark 在不同引擎下前缀不同，最终路径必不同（`camoufoxUserData_x` ≠ `clearcoteUserData_x`）。
- `isManagedUserDataFolder(name)` 只接受精确格式：
  - `camoufoxUserData` | `camoufoxUserData_<mark>`
  - `clearcoteUserData` | `clearcoteUserData_<mark>`
  - 不接受 `clearcoteUserDataEvil`、尾随空格、控制字符。
- `deleteDataFolders` / 列表 / 占用检查共用 canonical path：解析后必须仍在 `data/` 下；`lstat` 拒绝指向 `data/` 外的 symlink；跨引擎前缀伪造拒绝。非法目标报可读错误，不静默修正、不递归删除 `data/` 外内容。
- 配置加载与运行时解析都校验 mark（不只 WebUI）。

### [S2] fingerprint / preflight / 版本 / license 状态（P1-B）

- `fingerprintProfile` 非空时：
  - `options` 含 `fingerprintProfile`，**不含** `fingerprint` seed；
  - 不读取 seed 文件去覆盖 profile 语义（文件可保留）；
  - `seedSource = 'fingerprint-profile'`。
- 无 `fingerprintProfile` 时才 `ensureClearcoteSeed`，`seedSource = 'profile-seed'`。
- `fingerprintProfile` 做类型/可读性/路径错误校验；API/日志不回显文件内容或敏感参数。
- preflight：`process.platform`/`arch` 收敛到 win32-x64 / linux-x64；darwin 明确不支持；检查 SDK package + `launchPersistentContext` export + 版本 pin；显式 `path` 必须是可访问 regular file。
- runtime：
  - `browserVersion` ← 启动后 `browser.version()` 或 `RELEASE.version`；显式外部 executable 无法确认则 `unknown`/`unverified`；
  - `sdkVersion` 单独字段；
  - `fingerprintSource`：`fingerprint-profile` | `profile-seed` | `none`；
  - `license`：`free-requested` | `license-detected` | `unknown`，禁止硬编码“已证明 free”。

### [S2] sandbox 与免费边界（P1-C）

- 新增 `browser.clearcote.sandbox`: `true`（默认）| `false`。
- 默认安全：不自动添加 `--no-sandbox`。`sandbox: false` 时才注入，并在启动日志与 runtime.capabilities.sandboxEnabled=false 标明降低安全性。
- root + linux 且未显式关 sandbox 时，preflight/启动给出可操作错误或警告（指向 setuid chrome-sandbox 或显式配置）。
- 测试断言最终 `launchPersistentContext` options.args，而不是只测配置对象。
- 免费约束：
  - 不读取、打印、提交 license key；
  - 启动前 `licenseKeySource()`：`none` → `free-requested`；非 `none` → 拒绝继续并提示切换到干净 HOME/清 env（不删改用户已有 key）；可选配置 `allowDetectedLicense: true` 时才继续并标记 `license-detected`；
  - 隔离 HOME/XDG 的 fixture 测试 env/文件发现行为（不把真实 key 写入测试）。
- 免费并发：启动 Clearcote 前若 `getSessionSeats` 可用则检查；不可用则至少日志警告 + 失败可观测。不绕过席位限制。

### [S2] 关闭 / 恢复 / Worker 生命周期（P1-D）

- launcher 模块级 `lifecycle = { stopping: false, stopped: false }`；`cleanup()` 置 `stopping`，幂等可重复调用。
- `stopping|stopped` 时 context/browser/page close handler **禁止** `_reinit()`/新建 context。
- `Worker._reinit()` single-flight：并发调用共享同一 Promise。
- 释放顺序：pages → contexts → shared browser → proxy relay；单步失败记录但不阻塞后续。
- 共享 browser 派生 Worker 的 `engine`/`userDataDir`/runtime 一致。
- fake context/browser 测试：shutdown 后无新 launch；同一 close 不双 reinit；reinit 失败不留无主 active context。

### [S2] SOCKS5 与 relay 生命周期（P2-A）

- T0 结论：锁定 SDK **不能**原生完成 Chromium SOCKS5 认证 → **保留 relay**，更新“必须 relay”的表述为“SDK/Chromium 限制下必须 relay”。
- `user`/`passwd` 成对校验；半截凭据报错，不静默变无认证。
- `getClearcoteProxy` 返回 `{ proxy, release }` 或注册 context 绑定的 release handle：
  - 启动失败立即 release；
  - context close 时 release；
  - reinit 释放旧 relay；
  - 多 relay 引用计数或 Set + 显式 release（避免 cleanup 只关一半）；
  - `cleanupProxy` 幂等。
- 本地 fixture 测认证成功/失败/退出清理；不写真实外部凭据。

### [S2] 配置校验与 WebUI round-trip（P2-B）

- `clearcote` 必须是 object；逐字段类型校验；非法 engine/platform/mark/对象在 validator 报错。
- `args` 必须是 `string[]`；拒绝 `[1, null]`；拒绝或合并冲突的安全敏感参数（`--user-data-dir`、`--proxy-server`、`--headless`、`--remote-debugging-port`、fingerprint/profile 覆盖类）。
- WebUI args：**仅按行**解析（`split('\n')`），不再按逗号；`--disable-features=A,B` 整段保存/恢复。
- YAML ↔ API ↔ UI round-trip 测试。
- sandbox UI 只暴露安全可解释选项，并提示关闭风险。

### [S2] seed 可靠性（P2-C）

- 损坏/类型错误/字段缺失：**不**静默覆盖；保留原文件；可选拷贝到 `*.corrupt-<ts>.json` 隔离（不删证据）；返回可操作错误。
- 新 seed：临时文件 + `rename` 原子写入，mode `0o600`。
- 同 profile 首次并发创建：进程内 lock/single-flight，只产生一个 seed。
- 测试：正常、坏 JSON、缺字段、并发首建、写入中断。

### [S2] 测试与报告（P3 准备）

- `scripts/test-clearcote-hardening.mjs`：纯单元/fixture，覆盖上列断言 + 现有回归不破。
- `scripts/test-clearcote-e2e.mjs`：`CLEARCOTE_E2E=1` 才跑真实宿主机；否则 skip 且不写 PASS。
- macOS：只验证 preflight 拒绝；不把静态检查写成真实启动成功。
- `report.md`：纠正 stale “uncommitted” 与 head SHA；区分 observed / unverified / residual risks；不输出 secret/完整 profile 路径。

## [S3] Out of Scope

- 改 OpenAI/Anthropic 路由、模型 ID、适配器协议、上游站点逻辑。
- 运行时热切换引擎；静默降级普通 Chromium/Camoufox。
- 编译 Chromium、引入 PRO 私有二进制、购买/申请 license。
- 绕过 Clearcote 免费并发席位。
- 修改无关 worktree、主仓 `plan.md` / `计划.md`。
- 无 x64 宿主机时的假 E2E PASS。
- 默认 `git push`。

## Tasks

- [ ] T1: profile mark 与数据目录安全 — acceptance: `validateUserDataMark`/`isManagedUserDataFolder`/`resolveUserDataDirForEngine`/删除路径共享 canonical 规则；拒绝 `../`、绝对路径、`clearcoteUserDataEvil`、控制字符、越界 symlink；两引擎同 mark 路径不同；非法删除不触及 `data/` 外；测试绿 (covers: S2 profile)
- [ ] T2: fingerprint 互斥与 preflight/版本/license 状态 — acceptance: 有 `fingerprintProfile` 时 options 无 seed；无 profile 时持久 seed；`seedSource`/`browserVersion`/`sdkVersion`/`license` 状态可断言；darwin/非法 path/SDK pin 预检正确；不把 SDK 版本当浏览器版本 (covers: S2 fingerprint; depends: T1)
- [ ] T3: sandbox 与免费边界 — acceptance: 默认不传 `--no-sandbox`；显式关闭才注入且 runtime/日志标明；最终 launch options.args 有测试；license 非 `none` 时拒绝或需显式允许；不读不打印 key (covers: S2 sandbox; depends: T2)
- [ ] T4: shutdown/reinit 生命周期 — acceptance: `cleanup()` 幂等并置 stopping；shutdown 后 close 不 `_reinit`；`_reinit` single-flight；释放失败不阻塞；fake 测试覆盖三种场景 (covers: S2 lifecycle; depends: T2)
- [ ] T5: SOCKS5 relay 生命周期 — acceptance: 凭据成对校验；启动失败/context close/reinit/cleanup 释放 relay；fixture 认证成功与失败；文档更新为 SDK 限制下 relay (covers: S2 proxy; depends: T4)
- [ ] T6: 配置校验与 WebUI args 往返 — acceptance: clearcote 对象/args 类型校验；敏感参数拒绝；UI 仅按行解析且 `A,B` 无损 round-trip；validator 不抛未捕获异常 (covers: S2 config; depends: T3)
- [ ] T7: seed 原子性与损坏隔离 — acceptance: 坏 seed 不覆盖；原子写 0600；并发首建单 seed；测试绿 (covers: S2 seed; depends: T2)
- [ ] T8: hardening/e2e 脚本、文档与 report — acceptance: `test-clearcote-hardening.mjs` 全绿；`test-clearcote-e2e.mjs` 无 CLEARCOTE_E2E 时 skip；既有 browser/upgrade/ops/webui 回归绿；`report.md` 纠正 SHA/uncommitted 矛盾并区分 observed/unverified (covers: S2 测试与报告; depends: T1-T7)
