import fs from 'node:fs';
import net from 'node:net';
import readline from 'node:readline';

const socketPath = process.env.WEBAI2API_SOCK;
try { fs.unlinkSync(socketPath); } catch (error) { if (error.code !== 'ENOENT') throw error; }
const server = net.createServer(socket => {
    socket.write(`${JSON.stringify({ event: 'ready' })}\n`);
    const lines = readline.createInterface({ input: socket });
    lines.on('line', line => {
        let request;
        try { request = JSON.parse(line); } catch { return; }
        const { id, method, params = {} } = request;
        let result = {};
        if (method === 'preflight') result = { ok: true };
        else if (method === 'browser.start') result = {
            browserId: 'mock-browser', engine: 'mock', runtime: null, pageIds: ['mock-page']
        };
        else if (method === 'page.call') result = {
            results: (params.operations || []).map(operation => {
                if (operation.op === 'goto') return { url: operation.url, status: 200 };
                if (operation.op === 'evaluate' && operation.expression === 'document.bodyText') return '  203.0.113.42\n';
                return null;
            }), failedIndex: null, error: null
        };
        else if (method === 'cookies.get') result = { cookies: [] };
        else if (method === 'browser.close' || method === 'runtime.shutdown') result = { ok: true };
        socket.write(`${JSON.stringify({ id, result })}\n`);
    });
});
server.listen(socketPath, () => fs.chmodSync(socketPath, 0o600));
