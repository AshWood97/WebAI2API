/** Browser SDK startup/operation/shutdown smoke for macOS and Linux images. */
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { BrowserRuntime } from './browser-runtime.mjs';

process.env.WEBAI2API_SRC_ROOT ||= process.cwd();
const profile = await fs.mkdtemp(path.join(os.tmpdir(), 'webai2api-rs-browser-'));
const runtime = new BrowserRuntime();
const watchdog = setTimeout(() => {
    console.error('browser smoke timed out');
    process.exit(124);
}, 90000);

try {
    const config = { browser: { engine: 'camoufox', headless: true } };
    await runtime.preflight({ config });
    const started = await runtime.browserStart({
        config,
        options: { engine: 'camoufox', userDataDir: profile, instanceName: 'rs-smoke' }
    });
    const pageId = started.pageIds[0];
    if (!pageId) throw new Error('browser.start returned no page');
    const result = await runtime.pageCall({ pageId, operations: [
        { op: 'goto', url: 'about:blank' },
        { op: 'evaluate', expression: 'document.url' }
    ] });
    if (result.failedIndex !== null || result.results[1] !== 'about:blank') {
        throw new Error(`page operation failed: ${JSON.stringify(result)}`);
    }
    console.log('browser smoke passed');
} finally {
    clearTimeout(watchdog);
    await runtime.shutdown();
    await fs.rm(profile, { recursive: true, force: true });
}
