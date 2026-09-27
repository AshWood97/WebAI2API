import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import net from 'node:net';
import { EventEmitter } from 'node:events';
import { BrowserRuntime } from './browser-runtime.mjs';

class FakeLocator {
    async count() { return 1; }
    async isVisible() { return true; }
    async isEnabled() { return true; }
    async boundingBox() { return { x: 1, y: 2, width: 3, height: 4 }; }
    async scrollIntoViewIfNeeded() {}
    async waitFor() {}
    async innerText() { return 'ok'; }
    async getAttribute(name) { return name === 'href' ? '/ok' : null; }
    async click() {}
    async fill(value) { if (value === 'boom') throw new Error('fake fill failure'); }
    async press() {}
    first() { return this; }
    last() { return this; }
    nth() { return this; }
    filter() { return this; }
    async setInputFiles() {}
}

class FakePage extends EventEmitter {
    constructor() { super(); this.routes = []; this.keyboard = { press: async () => {}, type: async () => {} }; this.mouse = { click: async () => {}, move: async () => {}, wheel: async () => {} }; this.request = { get: async () => ({ ok: () => true, status: () => 200, body: async () => Buffer.from('x'), headers: () => ({}) }) }; }
    locator() { return new FakeLocator(); }
    getByRole() { return new FakeLocator(); }
    getByText() { return new FakeLocator(); }
    async goto(url) { this.currentUrl = url; return { url: () => url, status: () => 200 }; }
    url() { return this.currentUrl || 'about:blank'; }
    async title() { return 'fake'; }
    async waitForLoadState() {}
    async waitForTimeout() {}
    async screenshot() {}
    async evaluate(fn, args) { return fn.toString().includes('location.href') ? this.url() : args; }
    async close() { this.emit('close'); }
    async route(pattern, handler) { this.routes.push({ pattern, handler }); }
    async unroute(pattern, handler) { this.routes = this.routes.filter(route => route.pattern !== pattern || route.handler !== handler); }
    async triggerRoute() {
        const decision = { action: null };
        const route = {
            request: () => ({ url: () => 'https://site.test/', method: () => 'GET', headers: () => ({}), resourceType: () => 'document', postData: () => null }),
            continue: async () => { decision.action = 'continue'; },
            abort: async () => { decision.action = 'abort'; },
            fulfill: async () => { decision.action = 'fulfill'; }
        };
        await this.routes[0].handler(route);
        return decision;
    }
}

class FakeContext extends EventEmitter {
    constructor() { super(); this.openPages = []; }
    pages() { return this.openPages; }
    async newPage() { const page = new FakePage(); this.openPages.push(page); this.emit('page', page); return page; }
    async cookies() { return [{ name: 'session', value: 'test' }]; }
    async close() { for (const page of [...this.openPages]) await page.close(); this.emit('close'); }
}

function fixture(options = {}) {
    const contexts = [];
    const launcher = {
        initBrowserBase: async () => { const context = new FakeContext(); contexts.push(context); return { context, engine: 'fake' }; },
        cleanup: async () => {}
    };
    return { runtime: new BrowserRuntime({ launcher, routeTimeoutMs: 60, ...options }), contexts };
}

test('JSON-line socket frames fragmented and back-to-back requests', async t => {
    const { runtime } = fixture();
    const dir = await fs.promises.mkdtemp(path.join(os.tmpdir(), 'browser-runtime-'));
    const sockPath = path.join(dir, 'rpc.sock');
    const server = runtime.listen(sockPath);
    await new Promise(resolve => server.once('listening', resolve));
    const client = net.createConnection(sockPath);
    t.after(async () => { client.destroy(); await runtime.shutdown(); await fs.promises.rm(dir, { recursive: true, force: true }); });
    let buffer = '';
    const lines = [];
    client.setEncoding('utf8');
    client.on('data', chunk => { buffer += chunk; let i; while ((i = buffer.indexOf('\n')) >= 0) { lines.push(JSON.parse(buffer.slice(0, i))); buffer = buffer.slice(i + 1); } });
    await new Promise(resolve => client.once('connect', resolve));
    client.write('{"id":1,"method":"pre');
    client.write('flight","params":{}}\n{"id":2,"method":"browser.status","params":{}}\n');
    await new Promise(resolve => { const check = () => lines.length >= 3 ? resolve() : setTimeout(check, 5); check(); });
    assert.equal(lines[0].event, 'ready');
    assert.deepEqual(lines.slice(1).map(line => line.id), [1, 2]);
    assert.deepEqual(lines[1].result, { ok: true, skipped: true });
});

test('reconnected client never receives a reply from a disconnected request', async t => {
    const { runtime } = fixture();
    const dir = await fs.promises.mkdtemp(path.join(os.tmpdir(), 'browser-reconnect-'));
    const sockPath = path.join(dir, 'rpc.sock');
    const server = runtime.listen(sockPath);
    await new Promise(resolve => server.once('listening', resolve));
    const clients = [];
    t.after(async () => {
        for (const client of clients) client.destroy();
        await runtime.shutdown();
        await fs.promises.rm(dir, { recursive: true, force: true });
    });
    const pending = new Map();
    runtime.dispatch = request => new Promise(resolve => pending.set(request.params.tag, resolve));
    async function connect() {
        const client = net.createConnection(sockPath);
        clients.push(client);
        const messages = [];
        let buffer = '';
        client.setEncoding('utf8');
        client.on('data', chunk => {
            buffer += chunk;
            let end;
            while ((end = buffer.indexOf('\n')) >= 0) {
                messages.push(JSON.parse(buffer.slice(0, end)));
                buffer = buffer.slice(end + 1);
            }
        });
        await new Promise(resolve => client.once('connect', resolve));
        for (let tries = 0; tries < 100 && messages.length === 0; tries++) {
            await new Promise(resolve => setTimeout(resolve, 5));
        }
        assert.equal(messages[0]?.event, 'ready');
        return { client, messages };
    }
    const first = await connect();
    first.client.write(`${JSON.stringify({ id: 1, method: 'hold', params: { tag: 'old' } })}\n`);
    await new Promise(resolve => setImmediate(resolve));
    first.client.destroy();
    await new Promise(resolve => first.client.once('close', resolve));
    for (let tries = 0; tries < 100 && runtime.socket !== null; tries++) {
        await new Promise(resolve => setTimeout(resolve, 5));
    }
    assert.equal(runtime.socket, null);
    const second = await connect();
    second.client.write(`${JSON.stringify({ id: 1, method: 'hold', params: { tag: 'new' } })}\n`);
    await new Promise(resolve => setImmediate(resolve));
    pending.get('old')({ tag: 'old' });
    await new Promise(resolve => setTimeout(resolve, 20));
    assert.equal(second.messages.length, 1);
    pending.get('new')({ tag: 'new' });
    await new Promise(resolve => setTimeout(resolve, 20));
    assert.deepEqual(second.messages[1], { id: 1, result: { tag: 'new' } });
});

test('page.call preserves operation order and reports the failed index', async () => {
    const { runtime } = fixture();
    const { browserId } = await runtime.browserStart();
    const { pageId } = await runtime.pageCreate({ browserId });
    const result = await runtime.pageCall({ pageId, operations: [
        { op: 'goto', url: 'https://site.test' },
        { op: 'locator.fill', locator: { kind: 'css', value: '#prompt' }, value: 'boom' },
        { op: 'locator.click', locator: { kind: 'css', value: '#submit' } }
    ] });
    assert.equal(result.results.length, 1);
    assert.equal(result.failedIndex, 1);
    assert.match(result.error, /fake fill failure/);
    assert.equal(runtime.pages.get(pageId).page.url(), 'https://site.test');
    await runtime.shutdown();
});

test('new context page is registered once and exposes generic geometry', async () => {
    const { runtime, contexts } = fixture();
    const { browserId } = await runtime.browserStart();
    const popup = await contexts[0].newPage();
    const pageId = [...runtime.browsers.get(browserId).pages][0];
    assert.equal(runtime.pages.get(pageId).page, popup);
    const created = await runtime.pageCreate({ browserId });
    assert.equal(runtime.browsers.get(browserId).pages.size, 2);
    const result = await runtime.pageCall({ pageId: created.pageId, operations: [
        { op: 'locator.boundingBox', locator: { kind: 'css', value: 'main' } },
        { op: 'locator.scrollIntoViewIfNeeded', locator: { kind: 'css', value: 'main' } }
    ] });
    assert.deepEqual(result.results[0], { x: 1, y: 2, width: 3, height: 4 });
    assert.equal(result.failedIndex, null);
    await runtime.shutdown();
});

test('subscription is active before the following page action', async () => {
    const { runtime } = fixture();
    const { browserId } = await runtime.browserStart();
    const { pageId } = await runtime.pageCreate({ browserId });
    const { subscriptionId, afterSequence } = await runtime.eventSubscribe({ pageId, types: ['response'] });
    runtime.pages.get(pageId).page.emit('response', { url: () => 'https://site.test/api', status: () => 201 });
    const events = runtime.eventPoll({ afterSequence });
    assert.equal(events.events.length, 1);
    assert.equal(events.events[0].responseId.length > 0, true);
    assert.equal(events.events[0].pageId, pageId);
    assert.deepEqual(runtime.eventUnsubscribe({ subscriptionId }), { ok: true });
    await runtime.shutdown();
});

test('response finish wait is bounded and keyed by opaque response ID', async () => {
    const { runtime } = fixture();
    const { browserId } = await runtime.browserStart();
    const { pageId } = await runtime.pageCreate({ browserId });
    const page = runtime.pages.get(pageId).page;
    page.emit('response', { url: () => 'https://site.test/ok', status: () => 200, finished: async () => null });
    const completeId = [...runtime.responses.keys()][0];
    assert.deepEqual(await runtime.responseWaitFinished({ responseId: completeId, timeoutMs: 10 }), { ok: true });
    page.emit('response', { url: () => 'https://site.test/hang', status: () => 200, finished: () => new Promise(() => {}) });
    const stalledId = [...runtime.responses.keys()][1];
    await assert.rejects(runtime.responseWaitFinished({ responseId: stalledId, timeoutMs: 10 }), /timeout/);
    await runtime.shutdown();
});

test('bounded event buffer reports events dropped before the requested cursor', async () => {
    const { runtime } = fixture({ eventBufferLimit: 16 });
    const { browserId } = await runtime.browserStart();
    const { pageId } = await runtime.pageCreate({ browserId });
    const page = runtime.pages.get(pageId).page;
    for (let i = 0; i < 20; i++) page.emit('console', { type: () => 'log', text: () => String(i) });
    const batch = runtime.eventPoll({ afterSequence: 0 });
    assert.equal(batch.events.length, 16);
    assert.equal(batch.dropped, 6); // includes browser.started and page.created
    await runtime.shutdown();
});

test('page-scoped polling ignores unrelated page overflow but detects own loss', async () => {
    const { runtime } = fixture({ eventBufferLimit: 16 });
    const { browserId } = await runtime.browserStart();
    const first = await runtime.pageCreate({ browserId });
    const second = await runtime.pageCreate({ browserId });
    const cursor = runtime.eventSeq;
    const noisy = runtime.pages.get(second.pageId).page;
    for (let i = 0; i < 20; i++) noisy.emit('console', { type: () => 'log', text: () => String(i) });
    const quiet = runtime.pages.get(first.pageId).page;
    quiet.emit('response', { url: () => 'https://site.test/answer', status: () => 200 });
    const own = runtime.eventPoll({ afterSequence: cursor, pageId: first.pageId });
    assert.equal(own.dropped, 0);
    assert.equal(own.events.length, 1);
    for (let i = 0; i < 20; i++) noisy.emit('console', { type: () => 'log', text: () => `later-${i}` });
    assert.equal(runtime.eventPoll({ afterSequence: cursor, pageId: first.pageId }).dropped, 1);
    await runtime.shutdown();
});

test('runtime emits page and browser closure events for Rust lifecycle recovery', async () => {
    const { runtime } = fixture();
    const { browserId } = await runtime.browserStart();
    const { pageId } = await runtime.pageCreate({ browserId });
    const cursor = runtime.eventSeq;
    await runtime.pages.get(pageId).page.close();
    const pageEvents = runtime.eventPoll({ afterSequence: cursor }).events;
    assert.ok(pageEvents.some(event => event.type === 'page.closed' && event.pageId === pageId));
    assert.equal(runtime.browsers.get(browserId).pages.has(pageId), false);

    const beforeBrowserClose = runtime.eventSeq;
    await runtime.browsers.get(browserId).context.close();
    const closeEvents = runtime.eventPoll({ afterSequence: beforeBrowserClose }).events;
    assert.ok(closeEvents.some(event => event.type === 'browser.closed' && event.browserId === browserId));
    await runtime.shutdown();
});

test('route timeout defaults to continue and resolves opaque route token', async () => {
    const { runtime } = fixture({ routeTimeoutMs: 50 });
    const { browserId } = await runtime.browserStart();
    const { pageId } = await runtime.pageCreate({ browserId });
    const { routeId } = await runtime.routeInstall({ pageId, pattern: '**/*', timeoutMs: 50 });
    const page = runtime.pages.get(pageId).page;
    const decision = await page.triggerRoute();
    const routed = runtime.eventPoll({ afterSequence: 0 }).events.find(event => event.type === 'route');
    assert.equal(routed.url, 'https://site.test/');
    assert.equal(routed.request.method, 'GET');
    await new Promise(resolve => setTimeout(resolve, 70));
    assert.equal(decision.action, 'continue');
    assert.equal(runtime.routes.size, 1); // the installation handle remains until route.remove
    assert.deepEqual(await runtime.routeRemove({ routeId }), { ok: true });
    await runtime.shutdown();
});

test('browser/page cleanup closes contexts and removes records', async () => {
    const { runtime, contexts } = fixture();
    const { browserId } = await runtime.browserStart();
    const { pageId } = await runtime.pageCreate({ browserId });
    await runtime.browserClose({ browserId });
    assert.equal(contexts[0].openPages.length, 1); // fake context retains its historical pages list
    assert.equal(runtime.pages.has(pageId), false);
    assert.equal(runtime.browsers.has(browserId), false);
});

test('shutdown waits for an in-flight browser launch and closes its late context', async () => {
    let finishLaunch;
    let closed = false;
    const runtime = new BrowserRuntime({ launcher: {
        initBrowserBase: () => new Promise(resolve => { finishLaunch = resolve; }),
        cleanup: async () => {}
    } });
    const launch = runtime.browserStart({ config: { browser: { headless: true } } });
    await new Promise(resolve => setImmediate(resolve));
    const stopping = runtime.shutdown();
    finishLaunch({ context: { close: async () => { closed = true; } }, engine: 'fake' });
    await assert.rejects(launch, /shut down during browser startup/);
    await stopping;
    assert.equal(closed, true);
    assert.equal(runtime.browsers.size, 0);
});

test('evaluate accepts fixed expression names only and runtime imports no legacy backend', async () => {
    const { runtime } = fixture();
    const { browserId } = await runtime.browserStart();
    const { pageId } = await runtime.pageCreate({ browserId });
    const denied = await runtime.pageCall({ pageId, operations: [{ op: 'evaluate', expression: 'return userText', args: { userText: 'unsafe' } }] });
    assert.equal(denied.failedIndex, 0);
    const source = await fs.promises.readFile(new URL('./browser-runtime.mjs', import.meta.url), 'utf8');
    assert.match(source, /engine\/launcher\.js/);
    assert.doesNotMatch(source, /src\/backend|src\/server|PoolManager|registry\.js|adapters\//);
    await runtime.shutdown();
});
