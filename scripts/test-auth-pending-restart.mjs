import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const originalCwd = process.cwd();
const testRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'webai2api-auth-pending-'));

try {
    process.chdir(testRoot);
    fs.mkdirSync(path.join(testRoot, 'data'), { recursive: true });
    fs.writeFileSync(
        path.join(testRoot, 'data', 'config.yaml'),
        'server:\n  port: 5173\n  auth: pending-new-token\nqueue: {}\n',
        'utf8'
    );

    const { createAdminRouter } = await import('../src/server/api/admin/routes.js');
    const handleAdmin = createAdminRouter({
        config: { server: { auth: 'active-old-token' } },
        queueManager: {},
        tempDir: path.join(testRoot, 'temp')
    });
    const response = {
        writableEnded: false,
        writeHead(status, headers) {
            this.statusCode = status;
            this.headers = headers;
        },
        end(body) {
            this.body = body;
            this.writableEnded = true;
        }
    };

    await handleAdmin({ method: 'GET' }, response, '/config/server');
    const body = JSON.parse(response.body);
    assert.equal(response.statusCode, 200);
    assert.equal(body.authToken, 'active-old-token');
    assert.equal(body.authTokenPendingRestart, true);
    assert.equal(response.body.includes('pending-new-token'), false);
    console.log('PASS: server config GET hides pending auth token and reports restart state');
} finally {
    process.chdir(originalCwd);
    fs.rmSync(testRoot, { recursive: true, force: true });
}
