---
feature: camoufox-ops-docs
status: delivered
updated: 2026-09-20
branch: feat/camoufox-ops-docs
commits: afea55579d81aea7d84a1d7b2a7e7ba686237cfb..87a77a8
---

# Camoufox 运维配置补齐 + 文档调试体验

## Report

**What was built** — 3.9.0：Camoufox 运维面补齐（Docker `CAMOUFOX_INSTALL_DIR`、`locale` 透传、runtime 暴露内核版本、WebUI 浏览器能力表单且 humanize 默认 `camou`）与文档调试体验（`GET /docs` 自包含目录页、Dashboard 复制 Base URL/curl、Provider 健康卡片、WebUI API 文档入口）。`certificates`/`certificatePaths` **仅配置预留**：camoufox-js@0.12 未实现消费，CHANGELOG/example/WebUI 均已标注，避免误判为企业 CA 已可用。`/docs` 与 `/openapi.json` 同为只读目录、不 Bearer（浏览器 href 无法带 Authorization）。

**Verification** —
- `node scripts/test-ops-docs.mjs` → **12/12 PASS**（含 admin_token 键、HTML escape、certificates 预留语义）
- `test-camoufox-upgrade` 27/27、`test-upgrade` 29/29 PASS
- `vite build` PASS；dist 含 `Provider 状态` / `admin_token` / `预留`
- `readCamoufoxVersion` → `152.0.4-beta.30`
- 评审 critical 已修复：pnpm-workspace 占位符清理、`/docs` 鉴权与 UX、WebUI 不再清空 certificates、runtime 路径尊重 `CAMOUFOX_INSTALL_DIR`

**Journey log** —
1. camoufox-js **JS port** 无 certificates 实现；单测只测项目侧组装会误报「功能可用」。
2. pnpm 11 会往 `pnpm-workspace.yaml` 写 `allowBuilds: set this to true or false` 占位符；Docker COPY 后会污染构建。
3. 浏览器 `href` 无法带 Bearer → `/docs` 需与 `/openapi.json` 同策略或前端 fetch 打开。
4. WebUI token 键是 `admin_token`，不是 `webai2api_token`。
5. WebUI 保存 browser 配置时必须省略未编辑字段，避免清空 YAML 中的 certificates。

## [S1] Problem

main 已合并 FF152 基座与 3.7.0 协议层，但：

1. **Camoufox 运维面不完整**：Dockerfile 未设 `CAMOUFOX_INSTALL_DIR`；WebUI 浏览器设置未暴露 `ffVersion` / `browser.camoufox.*` 且 `humanizeCursor` 默认仍为 `false`；`/v1/runtime/status` 不报内核版本；`locale` / `certificates` 未接线（FF152 已修复 locale 生效）。
2. **文档与调试体验弱**：有 `/openapi.json` 无 `/docs`；WebUI 无一键复制 Base URL/curl；无 Provider 健康卡片；Playground 仅有基础接口测试抽屉，缺复制与文档入口。

用户决策：新 worktree `feat/camoufox-ops-docs`；Camoufox 运维/配置补齐包（A1–A4,A6）；文档调试体验包（B1,B2,B8,B9）。

## [S2] Design

### A. Camoufox 运维配置

| 契约 | 行为 |
|------|------|
| Dockerfile | `ENV CAMOUFOX_INSTALL_DIR=/app/camoufox`；确保 `scripts/camoufoxRelease.js` 随 `scripts/` 复制 |
| `browser.camoufox.locale` | string 或 string[]；launcher 透传 `locale` 给 Camoufox()；默认省略（跟指纹） |
| `browser.camoufox.certificates` / `certificatePaths` | 数组；有内容时透传 `certificates` / `certificatePaths` |
| `/v1/runtime/status` | 增加 `camoufox: { version, release, major, full }`（读 `PROJECT_CAMOUFOX_DIR/version.json`） |
| config 默认 | `locale: null`，`certificates: []`，`certificatePaths: []`；`humanizeCursor` WebUI 默认 `'camou'` |
| manager get/saveBrowserConfig | 读写 `ffVersion`、`camoufox.locale`、`camoufox.certificates`、`camoufox.certificatePaths` 及已有字段 |
| WebUI browser.vue | 表单字段：humanizeCursor 三态、ffVersion（可空）、locale、certificatePaths（多行文本）、mainWorldEval、enableCache、disableInstantAnimations、humanizeMaxTime、blockWebRtc、geoip；默认 humanize `'camou'` |

### B. 文档与调试体验

| 契约 | 行为 |
|------|------|
| `GET /docs` | 免鉴权或与 API 同鉴权？→ **同 API 鉴权**（有 token 时）；返回自包含 HTML，`fetch('/openapi.json')` 渲染端点目录（方法/路径/摘要/标签）；无外链 CDN |
| `GET /openapi.json` | 保持现有；HTML 页优先用同源 JSON |
| WebUI 复制 | Dash / 接口测试区：复制 `baseUrl`（`http://host/v1`）、复制 curl 示例（含 Authorization 头占位） |
| Provider 卡片 | Dash 或 tools 区：请求 `/v1/providers` 与 `/v1/auth/status`，展示适配器状态点（active/registered/idle）、Worker busy、模型数 |
| Playground | 在现有 api-test 抽屉上增强：生成 curl 预览 + 一键复制；菜单入口「API 文档」链到 `/docs` |

### 版本

- `package.json` → **3.9.0**
- CHANGELOG 增加 3.9.0

### 验证边界

- 单元：runtime status 含 camoufox 字段；locale/certificates 组装进 launch options；`/docs` HTML 含 openapi.json 引用
- 构建：`webui` vite build 产出 dist
- 冒烟：启动或至少 `node --check` + 单测；`/docs` 返回 HTML；runtime 含 camoufox（若 pool 未起仍可从 version.json 读）

## [S3] Out of Scope

- main_world_eval 适配器改造、idle 关浏览器、多 API Key、/metrics、/v1beta、文件附件
- Playwright 1.61（等 camoufox-js peer 放宽）
- 适配器 DOM 全量回归（需账号）
- hardware spoofing 默认启用

## Tasks

- [x] T1: Spec — acceptance: 本文件存在 (covers: S2)
- [x] T2: Dockerfile + camoufoxMeta/launcher locale/certificates + config/manager — acceptance: 启动 options 含配置项；Docker ENV 存在 (covers: S2)
- [x] T3: runtime/status 暴露 camoufox 版本 — acceptance: 字段结构正确 (covers: S2; depends: T2)
- [x] T4: /docs 自包含 HTML — acceptance: 返回可解析 openapi 的文档页（免 Bearer，与 openapi 一致）(covers: S2)
- [x] T5: WebUI browser 设置 + 复制 curl/BaseURL + Provider 卡片 + 文档入口 — acceptance: 源码含新字段且 vite build 成功 (covers: S2; depends: T2,T4)
- [x] T6: 单测 + 冒烟 + CHANGELOG 3.9.0 — acceptance: 测试通过 (covers: S2; depends: T3,T4,T5)
- [x] T7: 评审 critical 修复 + Finalize — acceptance: status=delivered；certificates 标注预留 (covers: S1,S2)
