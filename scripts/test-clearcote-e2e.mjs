/**
 * @fileoverview Clearcote real-host E2E (explicit switch required)
 *
 * Runs real launchPersistentContext only on Windows/Linux x64 with CLEARCOTE_E2E=1.
 * On macOS or without the switch: SKIP / UNVERIFIED — never claim PASS.
 * No real accounts, proxy passwords, license keys, or site cookies.
 *
 * Usage:
 *   CLEARCOTE_E2E=1 node scripts/test-clearcote-e2e.mjs
 *   $env:CLEARCOTE_E2E="1"; node scripts/test-clearcote-e2e.mjs
 */
import fs from 'fs';
import os from 'os';
import path from 'path';

const enabled = process.env.CLEARCOTE_E2E === '1';
const platform = process.platform;
const arch = process.arch;

function skip(reason) {
    console.log('SKIP: ' + reason);
    console.log('UNVERIFIED: Clearcote real-host E2E not executed on this machine.');
    process.exit(0);
}

if (!enabled) {
    skip('CLEARCOTE_E2E is not "1"');
}
if (platform === 'darwin') {
    skip('Clearcote official SDK does not support macOS (roadmap)');
}
if (platform !== 'win32' && platform !== 'linux') {
    skip('unsupported host platform: ' + platform);
}
if (arch !== 'x64' && arch !== 'x86_64') {
    skip('unsupported host arch: ' + arch);
}

const workDir = fs.mkdtempSync(path.join(os.tmpdir(), 'w2a-clearcote-e2e-'));
const profileA = path.join(workDir, 'clearcoteUserData_e2eA');
const profileB = path.join(workDir, 'clearcoteUserData_e2eB');

let passed = 0;
let failed = 0;

function ok(name) {
    passed++;
    console.log('  ok  ' + name);
}
function bad(name, e) {
    failed++;
    console.error('  FAIL ' + name + ': ' + (e && e.message ? e.message : e));
}

console.log('E2E host: ' + platform + '/' + arch);
console.log('workDir: ' + workDir);

const { buildClearcoteLaunchOptions, ensureClearcoteSeed } = await import(
    '../src/backend/engine/engineContract.js'
);
const { importClearcoteSdk, resolveLicenseBoundary, readClearcoteReleaseInfo } = await import(
    '../src/backend/engine/clearcoteMeta.js'
);

try {
    resolveLicenseBoundary({});
    ok('license boundary free-requested');
} catch (e) {
    bad('license boundary', e);
    console.log('Hint: unset CLEARCOTE_LICENSE_KEY and remove ~/.clearcote/license.key, or set allowDetectedLicense.');
    process.exit(1);
}

const sdk = await importClearcoteSdk();
const release = readClearcoteReleaseInfo(sdk);
console.log('SDK RELEASE browserVersion=' + release.browserVersion + ' tag=' + release.releaseTag);

async function launch(profileDir, extra) {
    const options = buildClearcoteLaunchOptions({
        browserConfig: { clearcote: Object.assign({ sandbox: true }, extra || {}) },
        userDataDir: profileDir,
        headless: true,
        hostPlatform: platform,
        hostArch: arch
    });
    const sdkOptions = Object.assign({}, options);
    delete sdkOptions.__meta;
    const context = await sdk.launchPersistentContext(profileDir, sdkOptions);
    return { context: context, sdkOptions: sdkOptions, meta: options.__meta };
}

try {
    const a1 = await launch(profileA);
    ok('persistent context launch A');

    let version = null;
    try {
        const br = a1.context.browser && a1.context.browser();
        if (br && br.version) version = br.version();
    } catch (e) { /* external binary may omit */ }
    console.log('  browser.version()=' + (version || 'unknown'));

    const page = await a1.context.newPage();
    await page.goto('about:blank');
    await a1.context.addCookies([{
        name: 'w2a_e2e',
        value: 'cookie-roundtrip',
        domain: '127.0.0.1',
        path: '/'
    }]);
    const cookies = await a1.context.cookies();
    if (!cookies.some((c) => c.name === 'w2a_e2e')) throw new Error('cookie write missing');
    ok('cookie write');

    await a1.context.close();
    ok('context close A');

    const a2 = await launch(profileA);
    const cookies2 = await a2.context.cookies('http://127.0.0.1');
    if (!cookies2.some((c) => c.name === 'w2a_e2e' && c.value === 'cookie-roundtrip')) {
        throw new Error('cookie not persisted across restart');
    }
    ok('cookie persistence across restart');

    const seedA = ensureClearcoteSeed(profileA);
    const seedB = ensureClearcoteSeed(profileB);
    if (seedA === seedB) throw new Error('two profiles must not share seed');
    ok('two profiles distinct seeds');

    await a2.context.close();
    ok('context close A2');

    const a3 = await launch(profileB);
    const p3 = await a3.context.newPage();
    await p3.goto('data:text/html,<title>w2a-e2e</title><h1>ok</h1>');
    const title = await p3.title();
    if (title !== 'w2a-e2e') throw new Error('unexpected title: ' + title);
    ok('minimal HTML navigation');
    await a3.context.close();
    ok('context close B');
} catch (e) {
    bad('core e2e', e);
}

if (process.env.W2A_E2E_SOCKS5_HOST) {
    console.log('SOCKS5 fixture env detected — extend here without logging secrets.');
} else {
    console.log('UNVERIFIED: real SOCKS5 auth (set W2A_E2E_SOCKS5_* to exercise).');
}

console.log('RESULT: passed=' + passed + ' failed=' + failed);
if (failed > 0) process.exit(1);
console.log('E2E completed on supported host. Review observed vs unverified in report.md.');
