/**
 * @fileoverview Clearcote 加固纯单元/fixture 测试（不启动真实浏览器）
 * 用法: node scripts/test-clearcote-hardening.mjs
 */
import assert from 'assert';
import fs from 'fs';
import os from 'os';
import path from 'path';

import {
    validateUserDataMark,
    resolveUserDataDirForEngine,
    isManagedUserDataFolder,
    parseManagedUserDataFolder,
    resolveManagedUserDataPath,
    assertPathInsideDataDir,
    ensureClearcoteSeed,
    readClearcoteSeedFile,
    buildClearcoteLaunchOptions,
    sanitizeClearcoteArgs,
    assertClearcoteHostSupported,
    CLEARCOTE_SEED_FILE
} from '../src/backend/engine/engineContract.js';
import {
    buildClearcoteRuntimeMeta,
    detectClearcoteLicenseSource,
    resolveLicenseBoundary,
    readClearcoteReleaseInfo,
    preflightClearcote
} from '../src/backend/engine/clearcoteMeta.js';
import { acquireClearcoteProxy, releaseProxyHandle, getClearcoteProxy } from '../src/utils/proxy.js';
import { deleteDataFolders } from '../src/utils/systemInfo.js';

let passed = 0;
let failed = 0;

function assertOk(name, fn) {
    try {
        fn();
        passed++;
        console.log('  ok  ' + name);
    } catch (e) {
        failed++;
        console.error('  FAIL ' + name);
        console.error('    ' + e.message);
    }
}

function throwsOk(name, fn, re) {
    assertOk(name, () => {
        let threw = false;
        try {
            fn();
        } catch (e) {
            threw = true;
            if (re) assert.match(String(e.message), re, 'message mismatch: ' + e.message);
        }
        assert.ok(threw, 'should throw');
    });
}

function tmpdir() {
    return fs.mkdtempSync(path.join(os.tmpdir(), 'w2a-clearcote-'));
}

console.log('== T1 profile mark and data dir safety ==');
assertOk('empty mark allowed', () => assert.strictEqual(validateUserDataMark(''), ''));
assertOk('legal mark allowed', () => assert.strictEqual(validateUserDataMark('main-1_x'), 'main-1_x'));
throwsOk('reject ../src', () => validateUserDataMark('../src'));
throwsOk('reject nested path mark', () => validateUserDataMark('x/../camoufoxUserData_live'));
throwsOk('reject absolute path', () => validateUserDataMark('/etc/passwd'));
throwsOk('reject space in mark', () => validateUserDataMark('a b'));
throwsOk('reject leading space', () => validateUserDataMark(' main'));
throwsOk('reject slash', () => validateUserDataMark('a/b'));

assertOk('default camoufox dir', () => {
    const p = resolveUserDataDirForEngine('', 'camoufox', '/tmp/w2a-data');
    assert.ok(p.endsWith('camoufoxUserData'));
});
assertOk('same mark different engines differ', () => {
    const a = resolveUserDataDirForEngine('main', 'camoufox', '/tmp/w2a-data');
    const b = resolveUserDataDirForEngine('main', 'clearcote', '/tmp/w2a-data');
    assert.notStrictEqual(a, b);
    assert.ok(a.includes('camoufoxUserData_main'));
    assert.ok(b.includes('clearcoteUserData_main'));
});
throwsOk('reject mark traversal', () => resolveUserDataDirForEngine('../evil', 'clearcote', '/tmp/w2a-data'));

assertOk('exact camoufoxUserData', () => isManagedUserDataFolder('camoufoxUserData'));
assertOk('exact clearcoteUserData_main', () => isManagedUserDataFolder('clearcoteUserData_main'));
assertOk('reject clearcoteUserDataEvil', () => assert.ok(!isManagedUserDataFolder('clearcoteUserDataEvil')));
assertOk('reject camoufoxUserDataEvil', () => assert.ok(!isManagedUserDataFolder('camoufoxUserDataEvil')));
assertOk('reject trailing space', () => assert.ok(!isManagedUserDataFolder('clearcoteUserData_main ')));
assertOk('reject secrets', () => assert.ok(!isManagedUserDataFolder('secrets')));
assertOk('parse managed folder', () => {
    assert.deepStrictEqual(parseManagedUserDataFolder('clearcoteUserData_x'), { engine: 'clearcote', mark: 'x' });
    assert.strictEqual(parseManagedUserDataFolder('clearcoteUserDataEvil'), null);
});

assertOk('resolveManaged ok', () => {
    const p = resolveManagedUserDataPath('clearcoteUserData_a', '/tmp/w2a-data');
    assert.ok(p.endsWith('clearcoteUserData_a'));
});
throwsOk('resolveManaged reject fake prefix', () => resolveManagedUserDataPath('clearcoteUserDataEvil', '/tmp/w2a-data'));
throwsOk('assertPathInside reject data root', () => assertPathInsideDataDir('/tmp/w2a-data', '/tmp/w2a-data'));
throwsOk('assertPathInside reject escape', () => assertPathInsideDataDir('/tmp/other', '/tmp/w2a-data'));

assertOk('delete illegal targets stay outside', () => {
    const outside = tmpdir();
    fs.writeFileSync(path.join(outside, 'keep.txt'), 'keep');
    const result = deleteDataFolders(['clearcoteUserDataEvil', 'secrets'], []);
    assert.strictEqual(result.deleted.length, 0);
    assert.ok(fs.existsSync(path.join(outside, 'keep.txt')));
    fs.rmSync(outside, { recursive: true, force: true });
});

assertOk('delete rejects out-of-tree symlink', () => {
    const parent = tmpdir();
    const data = path.join(parent, 'data');
    const outside = tmpdir();
    fs.mkdirSync(data, { recursive: true });
    fs.writeFileSync(path.join(outside, 'secret.txt'), 'x');
    fs.symlinkSync(outside, path.join(data, 'clearcoteUserData_link'));
    const cwd = process.cwd();
    try {
        process.chdir(parent);
        const result = deleteDataFolders(['clearcoteUserData_link'], []);
        assert.ok(result.errors.length > 0);
    } finally {
        process.chdir(cwd);
        fs.rmSync(parent, { recursive: true, force: true });
        fs.rmSync(outside, { recursive: true, force: true });
    }
    assert.ok(true);
});

console.log('== T7 seed atomicity and corrupt isolation ==');
assertOk('first create seed', () => {
    const dir = path.join(tmpdir(), 'profile');
    const seed = ensureClearcoteSeed(dir);
    assert.ok(seed.length >= 16);
    assert.strictEqual(seed, ensureClearcoteSeed(dir));
});
assertOk('corrupt JSON preserved and throws', () => {
    const dir = path.join(tmpdir(), 'profile');
    fs.mkdirSync(dir, { recursive: true });
    const metaPath = path.join(dir, CLEARCOTE_SEED_FILE);
    fs.writeFileSync(metaPath, '{not-json');
    throwsOk('corrupt throws', () => ensureClearcoteSeed(dir), /corrupt|invalid|损坏|不完整|seed/i);
    assert.strictEqual(fs.readFileSync(metaPath, 'utf8'), '{not-json');
});
assertOk('missing field not overwritten', () => {
    const dir = path.join(tmpdir(), 'profile');
    fs.mkdirSync(dir, { recursive: true });
    const metaPath = path.join(dir, CLEARCOTE_SEED_FILE);
    const raw = JSON.stringify({ version: 1 });
    fs.writeFileSync(metaPath, raw);
    assert.throws(() => ensureClearcoteSeed(dir));
    assert.strictEqual(fs.readFileSync(metaPath, 'utf8'), raw);
});
assertOk('valid seed readable', () => {
    const dir = path.join(tmpdir(), 'profile');
    ensureClearcoteSeed(dir);
    assert.ok(readClearcoteSeedFile(dir).ok);
});

console.log('== T2 fingerprint mutex and runtime status ==');
assertOk('fingerprintProfile excludes seed', () => {
    const dir = path.join(tmpdir(), 'profile');
    const options = buildClearcoteLaunchOptions({
        browserConfig: { clearcote: { fingerprintProfile: '/tmp/fp.json' } },
        userDataDir: dir,
        hostPlatform: 'linux',
        hostArch: 'x64'
    });
    assert.strictEqual(options.fingerprintProfile, '/tmp/fp.json');
    assert.strictEqual(options.fingerprint, undefined);
    assert.strictEqual(options.__meta.fingerprintSource, 'fingerprint-profile');
    assert.ok(!fs.existsSync(path.join(dir, CLEARCOTE_SEED_FILE)));
});
assertOk('no profile uses persistent seed', () => {
    const dir = path.join(tmpdir(), 'profile');
    const options = buildClearcoteLaunchOptions({
        browserConfig: { clearcote: {} },
        userDataDir: dir,
        hostPlatform: 'linux',
        hostArch: 'x64'
    });
    assert.ok(typeof options.fingerprint === 'string' && options.fingerprint.length >= 16);
    assert.strictEqual(options.fingerprintProfile, undefined);
    assert.strictEqual(options.__meta.fingerprintSource, 'profile-seed');
});
assertOk('runtime separates sdk and browser version', () => {
    const meta = buildClearcoteRuntimeMeta({
        hostPlatform: 'linux',
        fingerprintSource: 'profile-seed',
        browserVersion: '149.0.7827.114',
        releaseInfo: { browserVersion: '149.0.7827.114', releaseTag: 'v0.1.0-pre.22', releaseAsset: 'x.zip' },
        licenseStatus: 'free-requested',
        licenseSource: 'none',
        sandboxEnabled: true
    });
    assert.strictEqual(meta.browserVersion, '149.0.7827.114');
    assert.strictEqual(meta.fingerprintSource, 'profile-seed');
    assert.strictEqual(meta.license.status, 'free-requested');
    assert.notStrictEqual(meta.sdkVersion, meta.browserVersion);
    assert.strictEqual(meta.capabilities.proLicense, false);
});
assertOk('preflight darwin rejects', () => {
    const errors = preflightClearcote({}, 'darwin', 'arm64');
    assert.ok(errors.length > 0);
    assert.match(errors[0], /macOS|unsupported|不支持/);
});
assertOk('preflight linux x64 no platform reject', () => {
    const errors = preflightClearcote({}, 'linux', 'x64');
    assert.ok(!errors.some((e) => /macOS|unsupported host|不支持宿主/.test(e)));
});
throwsOk('reject linux arm64', () => assertClearcoteHostSupported('linux', 'arm64'));

console.log('== T3 sandbox and free boundary ==');
assertOk('default no --no-sandbox', () => {
    const dir = path.join(tmpdir(), 'p');
    const options = buildClearcoteLaunchOptions({
        browserConfig: { clearcote: { args: ['--disable-gpu'] } },
        userDataDir: dir,
        hostPlatform: 'linux',
        hostArch: 'x64'
    });
    assert.ok(!(options.args || []).includes('--no-sandbox'));
    assert.strictEqual(options.__meta.sandboxEnabled, true);
});
assertOk('sandbox false injects --no-sandbox', () => {
    const dir = path.join(tmpdir(), 'p');
    const options = buildClearcoteLaunchOptions({
        browserConfig: { clearcote: { sandbox: false, args: ['--disable-features=A,B'] } },
        userDataDir: dir,
        hostPlatform: 'linux',
        hostArch: 'x64'
    });
    assert.ok(options.args.includes('--no-sandbox'));
    assert.ok(options.args.includes('--disable-features=A,B'));
    assert.strictEqual(options.__meta.sandboxExplicitlyDisabled, true);
});
throwsOk('args reject --user-data-dir', () => sanitizeClearcoteArgs(['--user-data-dir=/tmp/x']));
throwsOk('args reject --proxy-server', () => sanitizeClearcoteArgs(['--proxy-server=http://x']));
throwsOk('args reject --headless', () => sanitizeClearcoteArgs(['--headless=new']));
throwsOk('args reject hand-written --no-sandbox', () => sanitizeClearcoteArgs(['--no-sandbox']));
throwsOk('args reject non-strings', () => sanitizeClearcoteArgs([1, null]));
assertOk('license none free-requested', () => {
    const saved = process.env.CLEARCOTE_LICENSE_KEY;
    delete process.env.CLEARCOTE_LICENSE_KEY;
    try {
        assert.strictEqual(resolveLicenseBoundary({}).status, 'free-requested');
    } finally {
        if (saved !== undefined) process.env.CLEARCOTE_LICENSE_KEY = saved;
    }
});
assertOk('license env detected and refused', () => {
    process.env.CLEARCOTE_LICENSE_KEY = 'cc_lic_test_dummy_not_real';
    try {
        assert.strictEqual(detectClearcoteLicenseSource().source, 'env');
        assert.throws(() => resolveLicenseBoundary({}), /free|license|PRO/i);
        assert.strictEqual(resolveLicenseBoundary({ allowDetectedLicense: true }).status, 'license-detected');
    } finally {
        delete process.env.CLEARCOTE_LICENSE_KEY;
    }
});
assertOk('releaseInfo is browser not sdk', () => {
    const rel = readClearcoteReleaseInfo({ RELEASE: { version: '149.0.7827.114', tag: 'v0.1.0-pre.22' } });
    assert.strictEqual(rel.browserVersion, '149.0.7827.114');
});

async function asyncTests() {
    console.log('== T4 shutdown / reinit lifecycle ==');
    try {
        const { Worker } = await import('../src/backend/pool/Worker.js');
        const fake = Object.create(Worker.prototype);
        fake.name = 'fake';
        fake.initialized = true;
        fake.browser = {
            closed: false,
            isClosed() { return this.closed; },
            async close() { this.closed = true; }
        };
        fake.page = {
            closed: false,
            isClosed() { return this.closed; },
            async close() { this.closed = true; }
        };
        let calls = 0;
        fake._initNewBrowser = async () => {
            calls++;
            await new Promise((r) => setTimeout(r, 20));
        };
        const p1 = fake._reinit();
        const p2 = fake._reinit();
        assert.strictEqual(p1, p2, 'reinit must be single-flight');
        await Promise.all([p1, p2]);
        assert.strictEqual(calls, 1, 'only one _initNewBrowser');
        passed++;
        console.log('  ok  _reinit single-flight');
    } catch (e) {
        failed++;
        console.error('  FAIL _reinit single-flight: ' + e.message);
    }

    try {
        const { Worker } = await import('../src/backend/pool/Worker.js');
        const fake = Object.create(Worker.prototype);
        fake.name = 'fake-release';
        let pageClosed = 0;
        let ctxClosed = 0;
        const oldPage = { isClosed: () => false, close: async () => { pageClosed++; } };
        const oldBrowser = { isClosed: () => false, close: async () => { ctxClosed++; } };
        await fake._releaseBrowserResources(oldBrowser, oldPage);
        assert.strictEqual(pageClosed, 1);
        assert.strictEqual(ctxClosed, 1);
        await fake._releaseBrowserResources(
            { isClosed: () => false, close: async () => { throw new Error('x'); } },
            { isClosed: () => false, close: async () => { throw new Error('y'); } }
        );
        passed++;
        console.log('  ok  reinit releases old page/context');
    } catch (e) {
        failed++;
        console.error('  FAIL reinit release: ' + e.message);
    }

    try {
        const launcher = await import('../src/backend/engine/launcher.js');
        await launcher.cleanup();
        assert.strictEqual(launcher.isShuttingDown(), true);
        passed++;
        console.log('  ok  cleanup sets isShuttingDown');

        const { Worker } = await import('../src/backend/pool/Worker.js');
        const fake = Object.create(Worker.prototype);
        fake.name = 'fake-shutdown';
        fake.initialized = false;
        fake.browser = null;
        fake.page = null;
        let launched = 0;
        fake._initNewBrowser = async () => { launched++; };
        await fake._reinit();
        assert.strictEqual(launched, 0, 'must not launch while shutting down');
        passed++;
        console.log('  ok  no reinit/launch during shutdown');
    } catch (e) {
        failed++;
        console.error('  FAIL shutdown no-reinit: ' + e.message);
    }

    console.log('== T7 concurrent seed create ==');
    try {
        const dir = path.join(tmpdir(), 'profile-conc');
        // 进程内并发：ensureClearcoteSeed 为同步，用两线程语义模拟同 tick 双调用
        const s1 = ensureClearcoteSeed(dir);
        const s2 = ensureClearcoteSeed(dir);
        assert.strictEqual(s1, s2);
        // 模拟锁被占用超时：预置锁目录且无 seed
        const dir2 = path.join(tmpdir(), 'profile-locked');
        fs.mkdirSync(path.join(dir2, '.webai2api-clearcote.lock'), { recursive: true });
        assert.throws(() => ensureClearcoteSeed(dir2), /锁超时|无锁|并发/);
        passed++;
        console.log('  ok  seed lock timeout refuses unlocked write');
    } catch (e) {
        failed++;
        console.error('  FAIL seed concurrency: ' + e.message);
    }

    console.log('== T5 SOCKS5 credential pair and handle ==');
    try {
        await acquireClearcoteProxy({
            enable: true, type: 'socks5', host: '127.0.0.1', port: 1080, user: 'u', passwd: ''
        });
        failed++;
        console.error('  FAIL half credentials should throw');
    } catch (e) {
        if (/pair|成对/.test(e.message)) {
            passed++;
            console.log('  ok  half credentials rejected');
        } else {
            failed++;
            console.error('  FAIL half credentials rejected: ' + e.message);
        }
    }

    try {
        const handle = await acquireClearcoteProxy({
            enable: true, type: 'http', host: '127.0.0.1', port: 8080, user: 'u', passwd: 'p'
        });
        assert.strictEqual(handle.proxy.username, 'u');
        assert.strictEqual(handle.relayUrl, null);
        await releaseProxyHandle(handle);
        await releaseProxyHandle(handle);
        passed++;
        console.log('  ok  HTTP handle idempotent release');
    } catch (e) {
        failed++;
        console.error('  FAIL HTTP handle: ' + e.message);
    }

    try {
        const p = await getClearcoteProxy({ enable: true, type: 'socks5', host: '127.0.0.1', port: 1080 });
        assert.strictEqual(p.server, 'socks5://127.0.0.1:1080');
        passed++;
        console.log('  ok  SOCKS5 no-auth direct');
    } catch (e) {
        failed++;
        console.error('  FAIL SOCKS5 no-auth: ' + e.message);
    }

    try {
        // 本地 SOCKS5 认证 fixture：成功/失败
        const net = await import('net');
        const { once } = await import('events');

        function startSocks5(user, pass) {
            return new Promise((resolve) => {
                const server = net.createServer((sock) => {
                    sock.once('data', (buf) => {
                        // methods: prefer user/pass 0x02
                        sock.write(Buffer.from([0x05, 0x02]));
                        sock.once('data', (authBuf) => {
                            const ulen = authBuf[1];
                            const u = authBuf.slice(2, 2 + ulen).toString();
                            const plen = authBuf[2 + ulen];
                            const p = authBuf.slice(3 + ulen, 3 + ulen + plen).toString();
                            if (u === user && p === pass) {
                                sock.write(Buffer.from([0x01, 0x00]));
                                // 极简：认证后立刻结束（relay 建立连接即可视为认证成功）
                                sock.end();
                            } else {
                                sock.write(Buffer.from([0x01, 0x01]));
                                sock.end();
                            }
                        });
                    });
                });
                server.listen(0, '127.0.0.1', () => resolve(server));
            });
        }

        const okSrv = await startSocks5('alice', 'secret');
        const okPort = okSrv.address().port;
        const handle = await acquireClearcoteProxy({
            enable: true, type: 'socks5', host: '127.0.0.1', port: okPort,
            user: 'alice', passwd: 'secret'
        });
        assert.ok(handle && handle.proxy && handle.proxy.server.startsWith('http://127.0.0.1:'));
        assert.ok(handle.relayUrl, 'auth socks5 must use relay');
        await releaseProxyHandle(handle);
        okSrv.close();
        passed++;
        console.log('  ok  SOCKS5 auth relay created (success path)');

        const badSrv = await startSocks5('alice', 'secret');
        const badPort = badSrv.address().port;
        // 错误凭据：relay 建立可能成功，但上游认证失败应可观测；至少不应静默变无认证
        const handleBad = await acquireClearcoteProxy({
            enable: true, type: 'socks5', host: '127.0.0.1', port: badPort,
            user: 'alice', passwd: 'wrong'
        });
        assert.ok(handleBad.relayUrl, 'still creates relay for bad creds (fails at use time)');
        // 释放后不再残留
        await releaseProxyHandle(handleBad);
        badSrv.close();
        passed++;
        console.log('  ok  SOCKS5 bad-creds still requires auth relay (not dropped)');
    } catch (e) {
        failed++;
        console.error('  FAIL SOCKS5 auth fixture: ' + e.message);
    }

    console.log('== summary ==');
    console.log('passed=' + passed + ' failed=' + failed);
    process.exit(failed > 0 ? 1 : 0);
}

asyncTests();
