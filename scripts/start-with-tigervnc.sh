#!/bin/sh
set -eu

# Keep X0tigervnc under the existing supervisor instead of starting a second
# display server. This mutates only the image filesystem at container startup.
node <<'NODE'
const fs = require('fs');
const target = '/app/supervisor.js';
let source = fs.readFileSync(target, 'utf8');

if (!source.includes("spawn('X0tigervnc'")) {
  source = source.replace(
    "if (!checkCommand('x11vnc')) {\n        log('WARN', '未找到 x11vnc 命令，跳过 VNC 启动');",
    "if (!checkCommand('X0tigervnc')) {\n        log('WARN', '未找到 X0tigervnc 命令，跳过 VNC 启动');"
  );

  const oldBlock = `const vncProcess = spawn('x11vnc', [
        '-display', display,
        '-rfbport', String(vncPort),
        '-localhost',
        '-nopw',
        '-shared',
        '-forever',
        '-noxdamage',
        '-norc',
        '-geometry', '1366x768'
    ], {`;
  const newBlock = `const vncProcess = spawn('X0tigervnc', [
        '-display', display,
        '-rfbport', String(vncPort),
        '-localhost',
        '-SecurityTypes', 'None',
        '-AlwaysShared'
    ], {`;

  if (!source.includes(oldBlock)) {
    throw new Error('WebAI2API supervisor VNC block did not match the supported upstream version');
  }

  source = source.replace(oldBlock, newBlock);
  fs.writeFileSync(target, source);
}
NODE

exec /usr/local/bin/docker-entrypoint.sh "$@"
