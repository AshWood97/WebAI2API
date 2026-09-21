VERDICT: IMPLEMENTED_WITH_UNVERIFIED_CLEARCOTE_LAUNCH

feature: dual-browser-engine-clearcote
branch: feat/dual-browser-engine-clearcote
worktree: /Users/a1-6/AI-Coding/WebAI2API/.worktrees/dual-browser-engine-clearcote
base_sha: 7eade9b4876c8dbb0402e6b51eff8df3fb7506a9
head: working tree (uncommitted; commit requires user authorization)
executor: MiMo compose-next (single implementer in isolated worktree)
sdk_pin: clearcote@0.30.0

## Modified / added files

- package.json, pnpm-lock.yaml, pnpm-workspace.yaml (pin clearcote@0.30.0; allowBuilds)
- src/backend/engine/engineContract.js (new)
- src/backend/engine/clearcoteMeta.js (new)
- src/backend/engine/launcher.js (dispatch + context Set)
- src/backend/pool/Worker.js, PoolManager.js
- src/config/index.js, manager.js, validator.js
- src/server/preflight.js, server.js, api/openai/routes.js
- src/utils/proxy.js, systemInfo.js
- webui browser.vue / workers.vue / dash.vue + dist
- config.example.yaml, README.md, README_EN.md, CHANGELOG.md, Dockerfile
- scripts/test-browser-engines.mjs (new)
- docs/compose/spec/dual-browser-engine-clearcote.md (new)

## Commands / exit codes

| Command | Exit | Result |
|---------|------|--------|
| pnpm install --frozen-lockfile | 0 | lockfile includes clearcote@0.30.0 |
| node --check launcher/Worker/PoolManager | 0 | PASS |
| node scripts/test-browser-engines.mjs | 0 | 77/77 PASS |
| node scripts/test-camoufox-upgrade.mjs | 0 | 27/27 PASS |
| node scripts/test-upgrade.mjs | 0 | 29/29 PASS |
| node scripts/test-ops-docs.mjs | 0 | 12/12 PASS |
| pnpm --dir webui build | 0 | vite build PASS; dist contains Clearcote UI strings |
| preflight engine=clearcote host=linux | 0 errors | SDK package resolved on disk |
| preflight engine=clearcote host=darwin | n/a | hard error, no Chromium fallback |

## Camoufox evidence

- Launch chain preserved (lazy-clearcote only on engine=clearcote).
- Old profile paths `data/camoufoxUserData*` unchanged when engine=camoufox.
- Existing upgrade/ops-docs suites green.
- Local Camoufox binary smoke not re-run in this worktree (camoufox/ assets not installed here); unit suites cover contract/UA/capability assembly.

## Clearcote evidence

- npm registry verified launchPersistentContext + Windows/Linux x64 + SHA-256 cache + free path.
- Option builder isolation tests: no firefox_user_prefs / webgl_config / camoufox blob / ffVersion.
- Persistent seed file `.webai2api-clearcote.json`, high-entropy, profile-scoped.
- Preflight on supported Linux host: no false “package unresolvable” after ESM path fix.
- **UNVERIFIED**: real Clearcote browser launch / cookie persistence on Windows/Linux x64.
- **UNVERIFIED**: macOS Clearcote launch — platform unsupported by design; tests assert hard error.

## Residual risks

1. Free-tier Clearcote concurrency default = 1 concurrent browser (upstream).
2. First Clearcote start may download verified binary (latency); production should pre-fetch via SDK on supported hosts.
3. pnpm 11 ignored-builds gate requires allowBuilds for esbuild/core-js (env workaround).
4. Real-site AI adapters not regression-tested on dual engines without accounts.

## Review

Initial review found critical: ESM `require.resolve('clearcote/package.json')` false-failed Clearcote preflight on Linux/Windows. Fixed to disk path + version pin check; nested runtime sanitize; multi-relay proxy cleanup; tests strengthened to 77/77. Focused re-review PASS — all four fixes verified, no new criticals.
