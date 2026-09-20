/**
 * Camoufox FF152 升级单元测试
 * 运行: node scripts/test-camoufox-upgrade.mjs
 */
import fs from 'fs';
import path from 'path';
import os from 'os';
import {
    readCamoufoxVersion,
    buildCamoufoxDownloadUrl,
    rewriteFingerprintUserAgent,
    migrateFingerprintFile,
    buildCamoufoxCapabilityOptions,
    camoufoxConfigKeySupported
} from '../src/backend/engine/camoufoxMeta.js';
import { CAMOUFOX_PATCHES } from './postinstall.js';
import { CAMOUFOX_RELEASE, CAMOUFOX_VERSION_JSON } from './camoufoxRelease.js';

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

console.log('\n[1] init 常量');
assert('CAMOUFOX_RELEASE = 152.0.4-beta.30', CAMOUFOX_RELEASE === '152.0.4-beta.30');
assert('version.json version', CAMOUFOX_VERSION_JSON.version === '152.0.4');
assert('version.json release', CAMOUFOX_VERSION_JSON.release === 'beta.30');

console.log('\n[2] 下载 URL');
{
    const mac = buildCamoufoxDownloadUrl(CAMOUFOX_RELEASE, 'darwin', 'arm64');
    const win = buildCamoufoxDownloadUrl(CAMOUFOX_RELEASE, 'win32', 'x64');
    assert('mac.arm64 URL', mac === 'https://github.com/daijro/camoufox/releases/download/v152.0.4-beta.30/camoufox-152.0.4-beta.30-mac.arm64.zip', mac);
    assert('win.x86_64 URL', win.includes('152.0.4-beta.30-win.x86_64.zip'), win);
}

console.log('\n[3] version.json 解析');
{
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'camoufox-ver-'));
    fs.writeFileSync(path.join(dir, 'version.json'), JSON.stringify(CAMOUFOX_VERSION_JSON));
    const ver = readCamoufoxVersion(dir);
    assert('major=152', ver?.major === 152);
    assert('full string', ver?.full === '152.0.4-beta.30');
    assert('missing dir null', readCamoufoxVersion(path.join(dir, 'nope')) === null);
    fs.rmSync(dir, { recursive: true, force: true });
}

console.log('\n[4] UA 迁移');
{
    const fp = { navigator: { userAgent: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:135.0) Gecko/20100101 Firefox/135.0' } };
    const { fingerprint, changed } = rewriteFingerprintUserAgent(fp, 152);
    assert('changed=true', changed === true);
    assert('rv 152', fingerprint.navigator.userAgent.includes('rv:152.0'));
    assert('Firefox/152', fingerprint.navigator.userAgent.includes('Firefox/152.0'));
    const again = rewriteFingerprintUserAgent(fingerprint, 152);
    assert('idempotent changed=false', again.changed === false);

    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'camoufox-fp-'));
    const file = path.join(dir, 'fingerprint.json');
    fs.writeFileSync(file, JSON.stringify({ navigator: { userAgent: 'Firefox/135.0 rv:135.0' } }));
    const mig = migrateFingerprintFile(file, 152);
    assert('file migrate ok', mig.ok && mig.changed);
    const saved = JSON.parse(fs.readFileSync(file, 'utf8'));
    assert('file saved UA 152', saved.navigator.userAgent.includes('152.0'));
    fs.rmSync(dir, { recursive: true, force: true });
}

console.log('\n[5] 能力选项组装');
{
    const fp = { screen: { availWidth: 1366, availHeight: 768 } };
    const opts = buildCamoufoxCapabilityOptions({
        humanizeCursor: 'camou',
        camoufox: { humanizeMaxTime: 2, mainWorldEval: true, enableCache: true }
    }, { major: 152, full: '152.0.4-beta.30' }, fp);
    assert('humanize=2', opts.humanize === 2);
    assert('main_world_eval', opts.main_world_eval === true);
    assert('enable_cache', opts.enable_cache === true);
    assert('window from fingerprint', Array.isArray(opts.window) && opts.window[0] === 1366);
    assert('block_webrtc default true', opts.block_webrtc === true);
    assert('no ff_version by default', opts.ff_version === undefined);
    assert('window still recorded for logs', Array.isArray(opts.window));

    const spoofed = buildCamoufoxCapabilityOptions({ ffVersion: 152, humanizeCursor: true }, null, {});
    assert('explicit ffVersion', spoofed.ff_version === 152);
    assert('i_know_what_im_doing', spoofed.i_know_what_im_doing === true);
    assert('true humanize not camou → no humanize key or falsey', !spoofed.humanize);

    const instant = buildCamoufoxCapabilityOptions({
        humanizeCursor: 'camou',
        camoufox: { disableInstantAnimations: true }
    }, null, {});
    assert('_disableInstantAnimations flag', instant._disableInstantAnimations === true);
}

console.log('\n[5b] properties 键检测');
{
    const props = path.join(process.cwd(), 'camoufox', 'Camoufox.app', 'Contents', 'MacOS', 'properties.json');
    if (fs.existsSync(props)) {
        assert('FF152 properties has disableInstantAnimations',
            camoufoxConfigKeySupported(props, 'disableInstantAnimations'));
    } else {
        console.log('  (skip) properties.json not installed in this cwd');
    }
}

console.log('\n[6] 补丁映射');
assert('only utils@0.12', Object.keys(CAMOUFOX_PATCHES).length === 1
    && CAMOUFOX_PATCHES['camoufox-js@0.12.0.utils.patched.js'] === 'utils.js');
assert('patch file exists', fs.existsSync(path.join(process.cwd(), 'patches', 'camoufox-js@0.12.0.utils.patched.js')));

console.log(`\n结果: ${passed} passed, ${failed} failed\n`);
process.exit(failed > 0 ? 1 : 0);
