#!/usr/bin/env node
/**
 * 版本同步：把 package.json 的 version 写入 WebAI2API-RS/Cargo.toml。
 * 用途：RS 二进制与 Node 版共享版本号（/health、/docs、镜像标签都用它）。
 *
 * 用法：
 *   node scripts/sync-rs-version.mjs          同步并报告
 *   node scripts/sync-rs-version.mjs --check  只校验（不一致则 exit 1，供 CI 使用）
 */
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const pkgPath = path.join(root, 'package.json');
const cargoPath = path.join(root, 'WebAI2API-RS', 'Cargo.toml');

const pkg = JSON.parse(fs.readFileSync(pkgPath, 'utf8'));
const cargo = fs.readFileSync(cargoPath, 'utf8');

const match = cargo.match(/^version = "([^"]+)"/m);
if (!match) {
    console.error(`sync-rs-version: 未在 ${cargoPath} 找到 version 字段`);
    process.exit(2);
}
const cargoVersion = match[1];

if (cargoVersion === pkg.version) {
    console.log(`sync-rs-version: 版本一致 (${pkg.version})`);
    process.exit(0);
}

if (process.argv.includes('--check')) {
    console.error(
        `sync-rs-version: 版本不一致！package.json=${pkg.version} Cargo.toml=${cargoVersion}\n` +
        `请运行: node scripts/sync-rs-version.mjs`
    );
    process.exit(1);
}

const updated = cargo.replace(/^version = "[^"]+"/m, `version = "${pkg.version}"`);
if (updated === cargo) {
    console.error('sync-rs-version: 替换未生效');
    process.exit(2);
}
fs.writeFileSync(cargoPath, updated);
console.log(`sync-rs-version: ${cargoVersion} -> ${pkg.version}`);
