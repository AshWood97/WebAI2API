---
feature: dual-browser-engine-clearcote-followup-hardening
status: delivered
updated: 2026-09-22
branch: feat/dual-browser-engine-clearcote
commits: 217ec656c2729eef6851dd2d58b36ce70db47d21..11ef457a5346166df7fe2a8e5b3f7dfce54f4a02
---

# Clearcote 双浏览器基座：执行后加固与验收

## Report

**What was built** — P1/P2 加固：profile 路径安全、fingerprint/seed 互斥、sandbox/license 边界、shutdown/reinit 生命周期、SOCKS5 relay、配置/WebUI round-trip、seed 原子性、hardening/e2e 脚本与 report.md。详见工作区 `report.md`。

**Verification** — hardening 55/55；browser-engines 77/77；camoufox 27/27；upgrade 29/29；ops-docs 12/12；webui build PASS；e2e SKIP/UNVERIFIED；独立评审 + residual 复审 PASS。

**Journey log** —
1. fingerprintProfile 与 seed：SDK 可共存，本项目按计划互斥。
2. SOCKS5：Chromium 不能原生认证，保留 relay。
3. seed 锁超时不得无锁写入。
4. reinit close 需 instance 身份守卫。
5. SOCKS5 测试必须经 relay 真请求。

## [S1] Problem

双引擎已交付但仍有 profile 路径穿越、fingerprint 语义、sandbox/license、shutdown reinit、seed 覆盖、args 切分、版本报告与 report 矛盾等 P1 缺口。

## [S2] Design

- Profile：validateUserDataMark + 精确托管名 + data/ 边界 + symlink 删除拒绝。
- fingerprint：有 profile 不传 seed；runtime 分离 sdk/browser 版本与 license 状态。
- sandbox：默认安全；sandbox:false 才注入 --no-sandbox；args 拒绝敏感参数。
- license：检测到来源则拒绝，除非 allowDetectedLicense。
- 生命周期：cleanup 幂等；_reinit single-flight；close 身份守卫；释放旧 context/relay。
- SOCKS5：保留 relay；凭据成对；handle 释放；fixture 真请求。
- 配置/UI：args 按行 round-trip。
- seed：损坏保留；原子写；锁超时拒绝写入。
- 测试：test-clearcote-hardening.mjs / test-clearcote-e2e.mjs；report 纠正 stale。

## [S3] Out of Scope

API/适配器改动、热切换、Chromium 编译、PRO、绕过席位、无关 worktree、假 E2E PASS、默认 push。

## Tasks

- [x] T1: profile mark 与数据目录安全
- [x] T2: fingerprint 互斥与 preflight/版本/license 状态
- [x] T3: sandbox 与免费边界
- [x] T4: shutdown/reinit 生命周期
- [x] T5: SOCKS5 relay 生命周期
- [x] T6: 配置校验与 WebUI args 往返
- [x] T7: seed 原子性与损坏隔离
- [x] T8: hardening/e2e 脚本、文档与 report
