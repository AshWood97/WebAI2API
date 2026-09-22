VERDICT: PASS_WITH_UNVERIFIED_E2E

feature: dual-browser-engine-clearcote-followup-hardening
branch: feat/dual-browser-engine-clearcote
worktree: /Users/a1-6/AI-Coding/WebAI2API/.worktrees/dual-browser-engine-clearcote
base_sha: 217ec656c2729eef6851dd2d58b36ce70db47d21
head_sha: 411538b36bf9df88c6a14539dfbdcf960a6eea84
executor: MiMo compose-next (single implementer in isolated worktree)
sdk_pin: clearcote@0.30.0
prior_review_target: feat/dual-browser-engine-clearcote@217ec656c2729eef6851dd2d58b36ce70db47d21

## Corrected stale claims

- Previous report.md said `head: working tree (uncommitted)`. That was already false at the dual-browser delivery commit `217ec65` (branch clean). This follow-up is committed on `feat/dual-browser-engine-clearcote` and is **not** left as an uncommitted working tree.
- SDK package version `0.30.0` is **not** the browser version. Browser version comes from `RELEASE.version` (149.0.7827.114) or `browser.version()` after launch.

## Changed paths (follow-up hardening)

- src/backend/engine/engineContract.js — mark/path safety, seed atomicity, fingerprint mutex, sandbox args
- src/backend/engine/clearcoteMeta.js — license boundary, browser vs sdk version, preflight arch/regular-file
- src/backend/engine/launcher.js — stopping/stopped lifecycle, proxy handle release, license/sandbox status
- src/backend/pool/Worker.js — single-flight `_reinit`, skip recovery while shutting down
- src/utils/proxy.js — credential pair check, acquire/release handle
- src/utils/systemInfo.js — exact managed folder + symlink-safe delete
- src/config/index.js, manager.js, validator.js — sandbox/allowDetectedLicense/args/mark validation
- webui/src/components/settings/browser.vue — args newline-only, sandbox/license toggles
- config.example.yaml — document sandbox/license/args semantics
- scripts/test-clearcote-hardening.mjs (new), scripts/test-clearcote-e2e.mjs (new)
- scripts/test-browser-engines.mjs — pass explicit x64 arch in synthetic linux/win32 cases
- docs/compose/spec/dual-browser-engine-clearcote-followup-hardening.md (new)

## Commands

| Command | Exit | Observed |
|---------|------|----------|
| node --check (engine/config/pool/utils/scripts) | 0 | PASS |
| node scripts/test-clearcote-hardening.mjs | 0 | 48/48 → 50/50 after T4 cases |
| node scripts/test-browser-engines.mjs | 0 | 77/77 PASS |
| node scripts/test-camoufox-upgrade.mjs | 0 | 27/27 PASS |
| node scripts/test-upgrade.mjs | 0 | 29/29 PASS |
| node scripts/test-ops-docs.mjs | 0 | 12/12 PASS |
| node scripts/test-clearcote-e2e.mjs | 0 | SKIP + UNVERIFIED (CLEARCOTE_E2E unset / macOS) |
| launcher cleanup smoke | 0 | isShuttingDown false→true, cleanup idempotent |

## observed

- `userDataMark` rejects `../`, absolute paths, spaces, control/space-trim, path separators.
- `isManagedUserDataFolder` is exact-format only (`clearcoteUserDataEvil` rejected).
- `resolveUserDataDirForEngine` confines paths under `data/` single-level; same mark → different engine paths.
- Delete path refuses fake prefixes and out-of-tree symlinks; does not touch files outside `data/`.
- Corrupt/incomplete seed file is preserved and throws; no silent identity overwrite.
- `fingerprintProfile` set → launch options have no auto seed; absent → persistent seed.
- Runtime separates `sdkVersion` / `browserVersion` / `fingerprintSource` / `license.status`.
- Default launch does not inject `--no-sandbox`; `sandbox: false` does and flags capabilities.
- License env detection refuses free path unless `allowDetectedLicense: true`.
- SOCKS5 half credentials rejected; HTTP handle release idempotent; SOCKS5 auth still uses relay (SDK/Chromium cannot natively auth SOCKS5).
- Worker `_reinit` is single-flight; launcher `cleanup()` sets shutting-down and is idempotent.

## unverified

- Real Clearcote `launchPersistentContext` on Windows x64 / Linux x64 (macOS host cannot run official SDK).
- Cookie persistence restart on real Clearcote binary.
- Dual-engine concurrent isolation under real Clearcote + Camoufox processes.
- Real SOCKS5 auth success/fail against a live proxy fixture.
- Root-container chrome-sandbox setuid behavior.
- Free-tier seat exhaustion behavior under multi Clearcote launch.

## p1_findings

- Addressed: path traversal / cross-engine delete risk, fingerprint+seed mutex, sandbox explicit policy, free/license auto-discovery refusal, shutdown reinit race.

## security_notes

- profile deletion: exact name + canonical `data/` boundary + symlink realpath check
- sandbox: default on; disable only via `browser.clearcote.sandbox: false`
- license/free mode: detect-only, never print/commit keys; refuse unless explicitly allowed
- proxy credentials: pair-validated; logs never print passwords; sanitizeRuntimeForApi strips them

## e2e_hosts

- linux-x64: UNVERIFIED (no host in this run)
- windows-x64: UNVERIFIED (no host in this run)
- macos: preflight rejects Clearcote (observed); real launch unsupported by design

## residual_risks

1. Free-tier Clearcote concurrency default remains 1 concurrent browser (upstream).
2. First Clearcote start may download verified binary (latency).
3. SOCKS5 auth still depends on proxy-chain relay lifecycle (not native Chromium auth).
4. Real-site AI adapters not regression-tested on dual engines without accounts.

## next_action

- Run `CLEARCOTE_E2E=1 node scripts/test-clearcote-e2e.mjs` on Windows x64 and Linux x64 hosts.
- Decide close action: keep branch / open PR / local merge (compose-next Finish).
