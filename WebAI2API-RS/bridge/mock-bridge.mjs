/**
 * 模拟引擎桥：实现与 bridge.mjs 相同的 JSON 行协议，但不启动浏览器。
 * 仅用于在没有浏览器内核的环境里验证 Rust 侧 HTTP/队列/协议链路。
 * 启动：node mock-bridge.mjs <socket路径>
 */
import net from 'net';
import fs from 'fs';

const sock = process.env.WEBAI2API_SOCK || process.argv[2];
if (!sock) { console.error('用法: WEBAI2API_SOCK=<socket> node mock-bridge.mjs'); process.exit(1); }

const models = {
    object: 'list',
    data: [
        { id: 'gpt-test', object: 'model', owned_by: 'mock', type: 'text', imagePolicy: 'optional' },
        { id: 'img-test', object: 'model', owned_by: 'mock', type: 'image', imagePolicy: 'required' }
    ]
};

async function handle(method, params) {
    switch (method) {
        case 'preflight':
            return { ok: true };
        case 'init':
            if (process.env.MOCK_INIT_FAIL === '1') throw new Error('模拟初始化失败');
            if (process.env.MOCK_INIT_STARTED_FILE) {
                fs.writeFileSync(process.env.MOCK_INIT_STARTED_FILE, String(process.pid));
            }
            if (process.env.MOCK_INIT_DELAY_MS) {
                await new Promise(r => setTimeout(r, Number(process.env.MOCK_INIT_DELAY_MS)));
            }
            return { ok: true, workers: [{ name: 'w1', engine: 'camoufox', instanceName: 'main', type: 'mock' }], browserCount: 1 };
        case 'getModels':
            return { object: 'list', data: models.data.map(m => ({ ...m, image_policy: m.imagePolicy })) };
        case 'modelInfo': {
            const want = params?.model;
            const found = want ? models.data.find(m => m.id === want) : null;
            return {
                models: { object: 'list', data: models.data.map(m => ({ ...m, image_policy: m.imagePolicy })) },
                modelType: found ? found.type : (want ? 'image' : null),
                imagePolicy: found ? found.imagePolicy : 'optional'
            };
        }
        case 'getModelType':
            return { type: models.data.find(m => m.id === params.model)?.type || 'image' };
        case 'getImagePolicy': {
            const found = models.data.find(m => m.id === params.model);
            return { policy: found ? found.imagePolicy : 'optional' };
        }
        case 'listAdapters':
            return { adapters: [{ id: 'mock', displayName: 'Mock', description: '模拟适配器', configSchema: [], models: models.data }] };
        case 'getCookies':
            if (params.instance && params.instance !== 'main') throw new Error('Worker 不存在');
            return { instance: params.instance || 'main', cookies: [{ name: 'sid', value: 'abc' }] };
        case 'navigateToMonitor':
            return { ok: true };
        case 'generate': {
            const prompt = params.prompt || '';
            if (prompt.includes('失败')) return { error: '生成失败(模拟)', code: 'CONTENT_BLOCKED', retryable: false };
            if (prompt.includes('crash-bridge')) {
                if (process.env.MOCK_DESCENDANT_MARKER) {
                    const marker = process.env.MOCK_DESCENDANT_MARKER;
                    const child = await import('node:child_process');
                    child.spawn(process.execPath, ['-e', `setTimeout(() => require('node:fs').writeFileSync(${JSON.stringify(marker)}, 'alive'), 1500); setInterval(() => {}, 1000)`], { stdio: 'ignore' });
                }
                process.exit(1);
            }
            if (prompt.includes('slow')) await new Promise(r => setTimeout(r, 1500));
            return { text: `echo:${prompt}`, reasoning: params.reasoning ? 'mock-reasoning' : undefined };
        }
        case 'downloadViaContext':
            return { path: '/tmp/mock.png', mime: 'image/png' };
        case 'shutdown':
            setTimeout(() => {
                try { fs.unlinkSync(sock); } catch {}
                process.exit(0);
            }, 50);
            return { ok: true };
        default:
            throw new Error(`未知方法: ${method}`);
    }
}

if (fs.existsSync(sock)) fs.unlinkSync(sock);
const server = net.createServer((socket) => {
    socket.write(JSON.stringify({ event: 'ready' }) + '\n');
    let buf = '';
    socket.on('data', (chunk) => {
        buf += chunk.toString('utf8');
        let idx;
        const lines = [];
        while ((idx = buf.indexOf('\n')) >= 0) {
            const line = buf.slice(0, idx).trim();
            buf = buf.slice(idx + 1);
            if (line) lines.push(line);
        }
        for (const line of lines) {
            const req = JSON.parse(line);
            handle(req.method, req.params || {}).then(
                (result) => socket.write(JSON.stringify({ id: req.id, result }) + '\n'),
                (e) => socket.write(JSON.stringify({ id: req.id, error: e.message }) + '\n')
            );
        }
    });
});
server.listen(sock, () => { fs.chmodSync(sock, 0o600); console.error(`mock bridge on ${sock}`); });
