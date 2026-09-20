/**
 * 3.9.0 运维/文档升级单元测试
 * 运行: node scripts/test-ops-docs.mjs
 */
import { buildCamoufoxCapabilityOptions } from '../src/backend/engine/camoufoxMeta.js';
import { buildDocsHtml } from '../src/server/api/docs.js';
import { readCamoufoxVersion } from '../src/backend/engine/camoufoxMeta.js';
import path from 'path';

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

console.log('\n[1] locale / certificates 启动选项');
{
    const opts = buildCamoufoxCapabilityOptions({
        humanizeCursor: 'camou',
        camoufox: {
            locale: 'zh-CN',
            certificatePaths: ['/tmp/ca.pem'],
            certificates: ['-----BEGIN CERTIFICATE-----']
        }
    }, null, {});
    assert('locale zh-CN', opts.locale === 'zh-CN');
    assert('certificatePaths', Array.isArray(opts.certificatePaths) && opts.certificatePaths[0] === '/tmp/ca.pem');
    assert('certificates raw', Array.isArray(opts.certificates) && opts.certificates.length === 1);

    const empty = buildCamoufoxCapabilityOptions({ camoufox: {} }, null, {});
    assert('locale omitted when unset', empty.locale === undefined);
    assert('certificates omitted when empty', empty.certificates === undefined);
}

console.log('\n[2] /docs HTML');
{
    const html = buildDocsHtml({ version: '3.9.0' });
    assert('contains openapi.json', html.includes('/openapi.json'));
    assert('contains version', html.includes('3.9.0'));
    assert('no external CDN script src', !/src=["']https?:\/\//.test(html));
    assert('has copy helpers', html.includes('copyVal') && html.includes('Base URL'));
    assert('reads admin_token', html.includes("localStorage.getItem('admin_token')"));
    assert('escapes HTML in client render', html.includes('function esc('));
}

console.log('\n[2b] certificates 仅为预留透传（JS port 未消费）');
{
    const opts = buildCamoufoxCapabilityOptions({
        camoufox: { certificatePaths: ['/tmp/ca.pem'] }
    }, null, {});
    assert('still passes certificatePaths for future', opts.certificatePaths?.[0] === '/tmp/ca.pem');
}

console.log('\n[3] runtime camoufox 版本读取');
{
    const ver = readCamoufoxVersion(path.join(process.cwd(), 'camoufox'));
    if (ver) {
        assert('major number', Number.isFinite(ver.major));
        assert('full string', typeof ver.full === 'string' && ver.full.length > 0);
        console.log('    camoufox:', ver.full);
    } else {
        console.log('  (skip) camoufox not installed in this cwd');
    }
}

console.log(`\n结果: ${passed} passed, ${failed} failed\n`);
process.exit(failed > 0 ? 1 : 0);
