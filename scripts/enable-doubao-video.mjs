/**
 * Idempotently enable the Doubao video Worker in a mounted data/config.yaml.
 *
 * This intentionally edits only the two known additive blocks and preserves
 * existing credentials, comments, account cookies and browser settings.
 */

import fs from 'fs';
import path from 'path';

const configPath = process.argv[2] || path.join(process.cwd(), 'data', 'config.yaml');
let source = fs.readFileSync(configPath, 'utf8');

if (!/^\s*- name:\s*["']?doubao_video["']?\s*$/m.test(source)) {
    const anchor = /^([ \t]*)- name:\s*["']?doubao_text["']?[ \t]*\n\1  type:\s*doubao_text[ \t]*$/m;
    if (!anchor.test(source)) {
        throw new Error('未在现有配置中找到 doubao_text Worker，拒绝猜测插入位置');
    }
    source = source.replace(anchor, (match, indent) => `${match}\n\n${indent}- name: "doubao_video"\n${indent}  type: doubao_video`);
}

if (!/^media:\s*$/m.test(source)) {
    source = `${source.trimEnd()}\n\n# 异步媒体任务配置（视频文件写入 data/media，元数据写入 SQLite）\nmedia:\n  retentionSeconds: 86400\n  maxBytes: 2147483648\n  maxUploadBytes: 20971520\n  pollIntervalMs: 10000\n  pollTimeoutMs: 2700000\n`;
}

const tempPath = `${configPath}.tmp-${process.pid}`;
fs.writeFileSync(tempPath, source, { mode: 0o600 });
fs.renameSync(tempPath, configPath);
console.log(JSON.stringify({ configPath, doubaoVideo: true, media: true }));
