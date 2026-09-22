/**
 * 双浏览器引擎契约单元测试
 * 运行: node scripts/test-browser-engines.mjs
 */
import fs from 'fs';
import os from 'os';
import path from 'path';
import { pathToFileURL } from 'url';

import {
    normalizeEngine,
    resolveInstanceEngine,
    resolveUserDataDirForEngine,
    browserShareKey,
    collectReferencedEngines,
    assertClearcoteHostSupported,
    resolveClearcoteFingerprintPlatform,
    ensureClearcoteSeed,
    buildClearcoteLaunchOptions,
    shouldUseGhostCursor,
    isManagedUserDataFolder,
    sanitizeRuntimeForApi,
    CLEARCOTE_SEED_FILE,
    CAMOUFOX_USERDATA_PREFIX,
    CLEARCOTE_USERDATA_PREFIX
} from '../src/backend/engine/engineContract.js';
import { preflightClearcote, readClearcoteSdkVersion, CLEARCOTE_SDK_PINNED } from '../src/backend/engine/clearcoteMeta.js';
import { getClearcoteProxy } from '../src/utils/proxy.js';
import { validateBrowserConfig, validateInstancesConfig } from '../src/config/validator.js';
import { resolveUserDataDir } from '../src/config/index.js';
import pkg from '../package.json' with { type: 'json' };

let passed = 0;
let failed = 0;
function assert(name, cond, detail = '') {
    if (cond) {
        passed++;
        console.log(`  ✓ ${name}`);
    } else {
        failed++;
        console.error(`  ✗ ${name}${detail ? ' — ' + detail : ''}`);
    }
}

console.log('\n[0] package pin');
{
    assert('clearcote pinned exact', pkg.dependencies?.clearcote === '0.30.0', String(pkg.dependencies?.clearcote));
    const installed = readClearcoteSdkVersion();
    if (installed) {
        assert('installed clearcote version', installed === CLEARCOTE_SDK_PINNED, installed);
    } else {
        console.log('  (skip) clearcote not resolvable in node_modules');
    }
}

console.log('\n[1] engine normalize / instance resolve');
{
    assert('default camoufox', normalizeEngine(undefined) === 'camoufox');
    assert('accept clearcote', normalizeEngine('clearcote') === 'clearcote');
    assert('lowercase', normalizeEngine('Camoufox') === 'camoufox');
    let threw = false;
    try { normalizeEngine('chromium'); } catch { threw = true; }
    assert('reject unknown engine', threw);

    const globalCfg = { browser: { engine: 'clearcote' } };
    assert('instance inherit global', resolveInstanceEngine(globalCfg, {}) === 'clearcote');
    assert('instance override', resolveInstanceEngine(globalCfg, { engine: 'camoufox' }) === 'camoufox');
}

console.log('\n[2] profile isolation paths');
{
    const base = '/tmp/w2a-data';
    const camou = resolveUserDataDirForEngine('', 'camoufox', base);
    const clear = resolveUserDataDirForEngine('', 'clearcote', base);
    assert('camoufox default path', camou === path.join(base, CAMOUFOX_USERDATA_PREFIX));
    assert('clearcote default path', clear === path.join(base, CLEARCOTE_USERDATA_PREFIX));
    assert('mark suffix camoufox', resolveUserDataDirForEngine('us1', 'camoufox', base).endsWith(`${CAMOUFOX_USERDATA_PREFIX}_us1`));
    assert('mark suffix clearcote', resolveUserDataDirForEngine('us1', 'clearcote', base).endsWith(`${CLEARCOTE_USERDATA_PREFIX}_us1`));
    assert('paths differ', camou !== clear);

    // config/index resolveUserDataDir 与契约一致
    const viaConfigCamou = resolveUserDataDir('mark1', 'camoufox');
    const viaConfigClear = resolveUserDataDir('mark1', 'clearcote');
    assert('config camoufox prefix', path.basename(viaConfigCamou) === `${CAMOUFOX_USERDATA_PREFIX}_mark1`);
    assert('config clearcote prefix', path.basename(viaConfigClear) === `${CLEARCOTE_USERDATA_PREFIX}_mark1`);

    assert('share key includes engine', browserShareKey('/x/y', 'clearcote') === 'clearcote::/x/y');
    assert('share keys differ by engine', browserShareKey('/x/y', 'camoufox') !== browserShareKey('/x/y', 'clearcote'));
}

console.log('\n[3] referenced engines + preflight filter');
{
    const dual = {
        browser: { engine: 'camoufox' },
        backend: {
            pool: {
                instances: [
                    { name: 'a', workers: [{ name: 'w1', type: 'test' }] },
                    { name: 'b', engine: 'clearcote', workers: [{ name: 'w2', type: 'test' }] }
                ]
            }
        }
    };
    const set = collectReferencedEngines(dual);
    assert('dual engines collected', set.has('camoufox') && set.has('clearcote'));

    const onlyClear = {
        browser: { engine: 'clearcote' },
        backend: { pool: { instances: [{ name: 'c', engine: 'clearcote', workers: [{ name: 'w', type: 'test' }] }] } }
    };
    const clearSet = collectReferencedEngines(onlyClear);
    assert('only clearcote', clearSet.size === 1 && clearSet.has('clearcote'));
}

console.log('\n[4] platform gate (macOS UNVERIFIED path)');
{
    let macErr = null;
    try { assertClearcoteHostSupported('darwin', 'arm64'); } catch (e) { macErr = e.message; }
    assert('darwin host rejected', !!macErr && macErr.includes('macOS'));
    assert('error mentions camoufox alternative', !!macErr && macErr.includes('camoufox'));
    assert(
        'error forbids chromium fallback claim',
        !!macErr && macErr.includes('不会将 Clearcote 静默回退为普通 Chromium')
    );

    let threw = false;
    try { assertClearcoteHostSupported('linux', 'x64'); } catch { threw = true; }
    assert('linux host allowed', !threw);
    threw = false;
    try { assertClearcoteHostSupported('win32', 'x64'); } catch { threw = true; }
    assert('win32 host allowed', !threw);

    assert('auto platform linux', resolveClearcoteFingerprintPlatform('auto', 'linux', 'x64') === 'linux');
    assert('auto platform windows', resolveClearcoteFingerprintPlatform('auto', 'win32', 'x64') === 'windows');
    assert('explicit persona on linux host', resolveClearcoteFingerprintPlatform('windows', 'linux', 'x64') === 'windows');

    const macPreflight = preflightClearcote({ platform: 'auto' }, 'darwin');
    assert('preflight clearcote on darwin errors', macPreflight.length > 0 && macPreflight[0].includes('macOS'));
    const linuxPreflight = preflightClearcote({ platform: 'auto', path: '' }, 'linux');
    assert('preflight clearcote on linux has no macOS error', !linuxPreflight.some(e => e.includes('macOS')));
    assert(
        'preflight clearcote on linux resolves SDK package on disk',
        !linuxPreflight.some(e => e.includes('未找到 clearcote') || e.includes('无法解析 clearcote')),
        linuxPreflight.join('; ')
    );
    assert(
        'preflight does not use broken ESM package.json resolve',
        !linuxPreflight.some(e => e.includes('无法解析 clearcote npm 包')),
        linuxPreflight.join('; ')
    );
}

console.log('\n[5] seed persistence + launch options isolation');
{
    const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'w2a-cc-'));
    const seed1 = ensureClearcoteSeed(tmp);
    const seed2 = ensureClearcoteSeed(tmp);
    assert('seed stable across calls', seed1 === seed2 && seed1.length >= 20);
    assert('seed metadata file exists', fs.existsSync(path.join(tmp, CLEARCOTE_SEED_FILE)));
    assert('seed is high entropy not instance name', !seed1.includes('browser_default') && seed1.startsWith('w2a-'));

    const other = fs.mkdtempSync(path.join(os.tmpdir(), 'w2a-cc2-'));
    const seedOther = ensureClearcoteSeed(other);
    assert('different profiles different seeds', seedOther !== seed1);

    const opts = buildClearcoteLaunchOptions({
        browserConfig: {
            engine: 'clearcote',
            ffVersion: 152,
            camoufox: { mainWorldEval: true },
            clearcote: {
                platform: 'linux',
                brand: 'Chrome',
                humanize: true,
                geoip: true,
                timezone: 'Asia/Shanghai',
                args: ['--disable-gpu'],
                path: ''
            }
        },
        userDataDir: tmp,
        headless: true,
        proxy: { server: 'http://127.0.0.1:7890' },
        hostPlatform: 'linux',
        hostArch: 'x64',
        explicitSeed: seed1
    });

    assert('options.headless', opts.headless === true);
    assert('options.fingerprint seed', opts.fingerprint === seed1);
    assert('options.platform', opts.platform === 'linux');
    assert('options.brand', opts.brand === 'Chrome');
    assert('options.humanize', opts.humanize === true);
    assert('options.geoip', opts.geoip === true);
    assert('options.timezone', opts.timezone === 'Asia/Shanghai');
    assert('options.args', Array.isArray(opts.args) && opts.args[0] === '--disable-gpu');
    assert('options.proxy object', opts.proxy?.server === 'http://127.0.0.1:7890');
    assert('no firefox_user_prefs', !('firefox_user_prefs' in opts));
    assert('no webgl_config', !('webgl_config' in opts));
    assert('no camoufox blob', !('camoufox' in opts));
    assert('no ffVersion', !('ffVersion' in opts));
    assert('no executablePath when empty path', !('executablePath' in opts));

    fs.rmSync(tmp, { recursive: true, force: true });
    fs.rmSync(other, { recursive: true, force: true });
}

console.log('\n[6] ghost-cursor policy');
{
    assert('camou mode no ghost', shouldUseGhostCursor('camoufox', { browser: { humanizeCursor: 'camou' } }) === false);
    assert('true + camoufox uses ghost', shouldUseGhostCursor('camoufox', { browser: { humanizeCursor: true } }) === true);
    assert('true + clearcote native humanize skips ghost', shouldUseGhostCursor('clearcote', {
        browser: { humanizeCursor: true, clearcote: { humanize: true } }
    }) === false);
    assert('true + clearcote humanize off uses ghost', shouldUseGhostCursor('clearcote', {
        browser: { humanizeCursor: true, clearcote: { humanize: false } }
    }) === true);
}

console.log('\n[7] proxy mapping');
{
    const http = await getClearcoteProxy({
        enable: true, type: 'http', host: '1.2.3.4', port: 8080, user: 'u', passwd: 'p'
    });
    assert('http proxy server', http.server === 'http://1.2.3.4:8080');
    assert('http proxy username', http.username === 'u');
    assert('http proxy password', http.password === 'p');

    const socksNoAuth = await getClearcoteProxy({
        enable: true, type: 'socks5', host: '5.6.7.8', port: 1080
    });
    assert('socks5 no auth server', socksNoAuth.server === 'socks5://5.6.7.8:1080');
    assert('socks5 no auth has no password field', !('password' in socksNoAuth));

    // SOCKS5 带认证：必须走 relay（返回本地 http），不得静默把 socks5+密码丢给 Chromium
    const socksAuth = await getClearcoteProxy({
        enable: true, type: 'socks5', host: '5.6.7.8', port: 1080, user: 'u', passwd: 'p'
    });
    assert('socks5 auth uses http relay not raw socks5', typeof socksAuth.server === 'string' && socksAuth.server.startsWith('http://'), socksAuth.server);
    assert('socks5 auth does not embed password in server', !String(socksAuth.server).includes('p@'));
}

console.log('\n[8] validator rejects bad engine / clearcote fields');
{
    const badEngine = validateBrowserConfig({ engine: 'chromium' });
    assert('browser.engine unknown rejected', !badEngine.valid && badEngine.errors.some(e => e.includes('engine')));

    const goodEngine = validateBrowserConfig({ engine: 'clearcote', clearcote: { platform: 'linux', brand: 'Chrome' } });
    assert('browser.engine clearcote accepted', goodEngine.valid, goodEngine.errors.join('; '));

    const badPlatform = validateBrowserConfig({ clearcote: { platform: 'macos' } });
    assert('clearcote.platform macos rejected', !badPlatform.valid);

    const forbidden = validateBrowserConfig({ clearcote: { ffVersion: 152 } });
    assert('clearcote ffVersion forbidden', !forbidden.valid);

    const badInst = validateInstancesConfig([
        { name: 'ok', workers: [{ name: 'w1', type: 'merge', mergeTypes: ['test'] }] },
        { name: 'bad', engine: 'chromium', workers: [{ name: 'w2', type: 'merge', mergeTypes: ['test'] }] }
    ]);
    // registry may not have adapters loaded in test; engine error should still appear if name/engine validated first
    const engineErr = badInst.errors.find(e => e.includes('engine'));
    assert('instance engine unknown rejected', !!engineErr, badInst.errors.join('; '));
}

console.log('\n[9] data folder prefix + runtime sanitize');
{
    assert('camoufox folder allowed', isManagedUserDataFolder('camoufoxUserData'));
    assert('clearcote folder allowed', isManagedUserDataFolder('clearcoteUserData_main'));
    assert('other folder denied', !isManagedUserDataFolder('secrets'));
    const sanitized = sanitizeRuntimeForApi({
        engine: 'clearcote',
        binaryPath: '/opt/clearcote/chrome',
        userDataDir: '/app/data/clearcoteUserData_x',
        proxyPassword: 'secret',
        proxyUsername: 'user',
        runtime: {
            engine: 'clearcote',
            binaryPath: '/opt/clearcote/chrome',
            userDataDir: '/app/data/clearcoteUserData_x',
            proxyPassword: 'nested-secret'
        }
    });
    assert('binary path basename only', sanitized.binaryPath === 'chrome');
    assert('userDataDir basename only', sanitized.userDataDir === 'clearcoteUserData_x');
    assert('no proxy password', !('proxyPassword' in sanitized));
    assert('nested runtime userDataDir basename', sanitized.runtime.userDataDir === 'clearcoteUserData_x');
    assert('nested runtime binaryPath basename', sanitized.runtime.binaryPath === 'chrome');
    assert('nested runtime no proxy password', !('proxyPassword' in sanitized.runtime));
}

console.log('\n[10] launcher syntax / exports');
{
    const launcherPath = path.join(process.cwd(), 'src/backend/engine/launcher.js');
    const launcher = await import(pathToFileURL(launcherPath).href);
    assert('initBrowserBase exported', typeof launcher.initBrowserBase === 'function');
    assert('cleanup exported', typeof launcher.cleanup === 'function');
    assert('shouldUseGhostCursor re-exported', typeof launcher.shouldUseGhostCursor === 'function');
}

console.log(`\n结果: ${passed} passed, ${failed} failed\n`);
process.exit(failed > 0 ? 1 : 0);
