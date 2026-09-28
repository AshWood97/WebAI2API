# Rust / Node WebUI final audit — 2026-09-28

VERDICT: PASS_WITH_EXTERNAL_E2E_FAILURE

Branch: `codex/rust-ui-final-audit`  
Base: `fc20a8ea950392f01cbb944b7b7079a5cfe24b7d`  
Environment: macOS, isolated Node and Rust data directories, Codex in-app browser.

## Scope and observed behavior

- Compared the same built WebUI against Node and Rust services. Nine routes loaded on both, including dashboard, server, worker, browser and adapter settings, display, cache, logs, and model request. Sixteen sampled GET endpoints returned 200 on each service; both exposed 19 adapters, 112 models and 2 providers.
- In the in-app browser, valid browser configuration save produced one success message and survived refresh. An invalid Clearcote argument was rejected without a success message. Rust originally returned startup-cached values after save; the six configuration GET routes now read the saved YAML while runtime settings still wait for restart.
- Checked desktop and 390 px mobile layouts. The browser settings form's narrow fixed columns were unusable at 390 px; all 34 columns now use responsive spans. Server settings, adapters and request pages remained within the mobile viewport.
- The dashboard now distinguishes the installed Camoufox kernel from configured engines and reports each worker's engine and readiness. It does not claim an unverified SDK-inferred number as a browser version. The app also removes its resize listener and connection timer on unmount.
- Config saves show the backend's restart-required message. Node and Rust server settings return the active authentication token and a pending-restart flag, never the pending replacement token. The UI shows that state and can cancel only the pending token change without saving other draft fields.

## Verification

| Check | Result |
| --- | --- |
| `cargo test --manifest-path WebAI2API-RS/Cargo.toml` | 138 unit and 12 integration tests passed |
| `cargo fmt --manifest-path WebAI2API-RS/Cargo.toml -- --check` | Passed |
| `cargo build --manifest-path WebAI2API-RS/Cargo.toml` | Passed |
| `node scripts/test-browser-engines.mjs` | 87 passed |
| `node scripts/test-browser-close-semantics.mjs` | 45 passed |
| `node --test WebAI2API-RS/bridge/browser-runtime.test.mjs` | 13 passed |
| `node scripts/test-auth-pending-restart.mjs` | Passed |
| `npm --prefix webui run build` | Passed; existing npm configuration and large bundle warnings |
| `git diff --check` | Passed |

The Rust integration suite verifies that saved configuration is visible in all six GET views while runtime remains on its startup values. It also verifies that the old token cannot read the pending new token, and that restart rejects the old token and accepts the new one. A lock test verifies that a GET waits for a save transaction's rollback. In-app browser testing confirmed the pending warning, hidden token, saving unrelated fields without an empty-token warning, and the cancellation confirmation's keep/cancel paths. Cancellation left an unrelated queue-size draft unsaved.

## External end-to-end limit

One `mai-image-2` image request with the same prompt was sent through each service using cloned Camoufox profiles. Neither completed: Node recorded `CLICK_TIMEOUT` on the LMArena textarea after site navigation; Rust recorded `API_TIMEOUT: 等待响应超时 (120秒)`. The public LMArena page opened in the in-app browser, but that does not establish the automation path. This audit therefore does not claim real-site generation success. Clearcote binary launch is unsupported on this macOS host and remains unverified; the existing project report likewise marks Linux/Windows real-binary E2E as outstanding.

## Final follow-up

1. On a supported Linux x64 or Windows x64 host, run the documented Clearcote binary/profile smoke and verify concurrent Camoufox/Clearcote instances.
2. Reproduce the LMArena adapter failure with browser tracing on a stable test account and fix the live selector/model-selection flow if the site still behaves the same way.
3. Review and merge this isolated branch after CI. The live-site request remains a named acceptance exception until it passes.

## Orchestration evidence

Native role packets: explorer (`repo_map`), worker (`ui_fix`), tester (`verify_final`), reviewer (`review_final`). Requested role profiles were Luna max for explorer/worker and Sol xhigh for tester/reviewer; the observed runtime model/provider is unavailable. No persistent runner was launched. The root integrated changes and performed the in-app browser comparison and live UI checks.
