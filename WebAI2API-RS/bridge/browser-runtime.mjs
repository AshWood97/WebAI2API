/**
 * Generic browser runtime RPC for the Rust server.
 *
 * This module deliberately knows nothing about adapters, workers, or the legacy
 * backend. It only starts browser contexts and exposes generic Playwright-like
 * primitives over a Unix-domain JSON-lines socket.
 */
import net from 'node:net';
import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { randomUUID } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import os from 'node:os';
import crypto from 'node:crypto';
import { collectReferencedEngines } from './engine/engineContract.js';
import { preflightClearcote } from './engine/clearcoteMeta.js';
import { readCamoufoxVersion } from './engine/camoufoxMeta.js';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const MAX_LINE_BYTES = 8 * 1024 * 1024;
const EVENT_BUFFER_LIMIT = 1024;
const DEFAULT_ROUTE_TIMEOUT_MS = 1500;
const MAX_ROUTE_TIMEOUT_MS = 30000;

function errorMessage(error) { return error?.message || String(error); }
function opaqueId() { return randomUUID(); }
function safeJson(value) {
    return JSON.parse(JSON.stringify(value, (_key, item) => typeof item === 'bigint' ? String(item) : item));
}

export class BrowserRuntime {
    constructor({ launcher = null, preflight = null, eventBufferLimit = EVENT_BUFFER_LIMIT, routeTimeoutMs = DEFAULT_ROUTE_TIMEOUT_MS } = {}) {
        this.launcher = launcher;
        this.preflightFn = preflight;
        this.eventBufferLimit = Math.max(16, eventBufferLimit);
        this.routeTimeoutMs = Math.min(MAX_ROUTE_TIMEOUT_MS, Math.max(25, routeTimeoutMs));
        this.browsers = new Map();
        this.pages = new Map();
        this.elements = new Map();
        this.frames = new Map();
        this.routes = new Map();
        this.responses = new Map();
        this.downloads = new Set();
        this.subscriptions = new Map();
        this.events = [];
        this.eventSeq = 0;
        this.droppedEvents = 0;
        this.server = null;
        this.socket = null;
        this.closed = false;
        this.handlers = {
            preflight: p => this.preflight(p),
            'browser.start': p => this.browserStart(p),
            'browser.close': p => this.browserClose(p),
            'browser.restart': p => this.browserRestart(p),
            'browser.status': p => this.browserStatus(p),
            'page.create': p => this.pageCreate(p),
            'page.close': p => this.pageClose(p),
            'page.call': p => this.pageCall(p),
            'event.subscribe': p => this.eventSubscribe(p),
            'event.unsubscribe': p => this.eventUnsubscribe(p),
            'event.poll': p => this.eventPoll(p),
            'response.body': p => this.responseBody(p),
            'response.waitFinished': p => this.responseWaitFinished(p),
            'route.install': p => this.routeInstall(p),
            'route.resolve': p => this.routeResolve(p),
            'route.remove': p => this.routeRemove(p),
            'download.fetch': p => this.downloadFetch(p),
            'cookies.get': p => this.cookiesGet(p),
            'runtime.shutdown': () => this.shutdown()
        };
    }

    async #launcher() {
        if (this.launcher) return this.launcher;
        const root = process.env.WEBAI2API_SRC_ROOT;
        if (!root) throw new Error('WEBAI2API_SRC_ROOT is required to start a browser');
        process.env.CAMOUFOX_INSTALL_DIR ||= path.join(root, 'camoufox');
        const launcherUrl = pathToFileURL(path.join(HERE, 'engine/launcher.js')).href;
        this.launcher = await import(launcherUrl);
        return this.launcher;
    }

    async preflight({ config } = {}) {
        if (this.preflightFn) return await this.preflightFn();
        const root = process.env.WEBAI2API_SRC_ROOT;
        if (!root) return { ok: true, skipped: true };
        const engines = collectReferencedEngines(config || { browser: { engine: 'camoufox' } });
        const errors = [];
        if (engines.has('camoufox')) {
            const browserDir = path.join(root, 'camoufox');
            const executable = process.platform === 'darwin'
                ? path.join(browserDir, 'Camoufox.app', 'Contents', 'MacOS', 'camoufox')
                : path.join(browserDir, process.platform === 'win32' ? 'camoufox.exe' : 'camoufox');
            if (!fs.existsSync(executable)) errors.push('Camoufox 可执行文件缺失，请运行: npm run init');
            const version = readCamoufoxVersion(browserDir);
            if (!version) errors.push('camoufox/version.json 缺失或无法解析，请运行: npm run init');
            else if (version.major < 146) errors.push(`Camoufox 内核过旧 (Firefox ${version.major})`);
            if (!fs.existsSync(path.join(browserDir, 'GeoLite2-City.mmdb'))) errors.push('camoufox/GeoLite2-City.mmdb 缺失，请运行: npm run init');
            const patch = path.join(root, 'patches', 'camoufox-js@0.12.0.utils.patched.js');
            const installed = path.join(root, 'node_modules', 'camoufox-js', 'dist', 'utils.js');
            if (fs.existsSync(patch)) {
                const digest = file => fs.existsSync(file) ? crypto.createHash('md5').update(fs.readFileSync(file)).digest('hex') : null;
                if (digest(patch) !== digest(installed)) errors.push('camoufox-js 补丁未应用，请运行: pnpm install');
            }
        }
        if (engines.has('clearcote')) errors.push(...preflightClearcote(config?.browser?.clearcote || {}, os.platform()));
        if (errors.length) throw new Error(`启动预检失败: ${errors.join('; ')}`);
        return { ok: true };
    }

    async browserStart(params = {}) {
        if (this.closed) throw new Error('runtime is shutting down');
        const mod = await this.#launcher();
        const config = params.config || {};
        const options = params.options || {};
        const id = opaqueId();
        const started = await mod.initBrowserBase(config, options);
        if (!started?.context) throw new Error('launcher returned no browser context');
        const record = { id, context: started.context, engine: started.engine || options.engine || config?.browser?.engine || 'camoufox', runtime: started.runtime || null, options, config, pages: new Set(), closed: false };
        this.browsers.set(id, record);
        record.context.on?.('page', page => this.#addPage(record, page));
        record.context.on?.('close', () => {
            record.closed = true;
            for (const pageId of [...record.pages]) this.#forgetPage(pageId, 'browser-closed');
            this.#emit({ type: 'browser.closed', browserId: id });
        });
        this.#emit({ type: 'browser.started', browserId: id, engine: record.engine });
        return { browserId: id, engine: record.engine, runtime: record.runtime, pageIds: this.#rememberExistingPages(record) };
    }

    #rememberExistingPages(browser) {
        const ids = [];
        for (const page of browser.context.pages?.() || []) ids.push(this.#addPage(browser, page).id);
        return ids;
    }

    #addPage(browser, page) {
        const existing = [...browser.pages].map(id => this.pages.get(id)).find(p => p?.page === page);
        if (existing) return existing;
        const record = { id: opaqueId(), browserId: browser.id, browser, page, generation: 1, closed: false, hooks: [] };
        this.pages.set(record.id, record);
        browser.pages.add(record.id);
        page.on?.('response', response => this.#captureResponse(record, response));
        page.on?.('close', () => this.#forgetPage(record.id, 'page-closed'));
        page.on?.('console', msg => this.#emit({ type: 'page.console', browserId: browser.id, pageId: record.id, level: msg.type?.(), text: msg.text?.() }));
        page.on?.('pageerror', error => this.#emit({ type: 'page.error', browserId: browser.id, pageId: record.id, error: errorMessage(error) }));
        this.#emit({ type: 'page.created', browserId: browser.id, pageId: record.id, generation: record.generation });
        return record;
    }

    #forgetPage(pageId, reason) {
        const page = this.pages.get(pageId);
        if (!page) return;
        page.closed = true;
        page.generation++;
        for (const hook of page.hooks) { try { hook(); } catch {} }
        page.browser.pages.delete(pageId);
        this.pages.delete(pageId);
        for (const [id, element] of this.elements) if (element.pageId === pageId) {
            this.elements.delete(id);
            Promise.resolve(element.handle.dispose?.()).catch(() => {});
        }
        for (const [id, frame] of this.frames) if (frame.pageId === pageId) this.frames.delete(id);
        this.#emit({ type: 'page.closed', browserId: page.browserId, pageId, generation: page.generation, reason });
        for (const [id, sub] of this.subscriptions) if (sub.pageId === pageId) this.eventUnsubscribe({ subscriptionId: id });
    }

    async browserClose({ browserId } = {}) {
        const browser = this.#getBrowser(browserId);
        for (const pageId of [...browser.pages]) await this.pageClose({ pageId });
        try { await browser.context.close(); } finally {
            browser.closed = true;
            this.browsers.delete(browser.id);
            this.#emit({ type: 'browser.closed', browserId: browser.id });
        }
        return { ok: true };
    }

    async browserRestart({ browserId } = {}) {
        const old = this.#getBrowser(browserId);
        const { config, options } = old;
        await this.browserClose({ browserId });
        const started = await this.browserStart({ config, options });
        return { ...started, previousBrowserId: browserId };
    }

    browserStatus({ browserId } = {}) {
        if (browserId) {
            const b = this.#getBrowser(browserId);
            return { browserId, engine: b.engine, closed: b.closed, pageIds: [...b.pages] };
        }
        return { browsers: [...this.browsers.values()].map(b => ({ browserId: b.id, engine: b.engine, closed: b.closed, pageIds: [...b.pages] })), pages: this.pages.size };
    }

    async pageCreate({ browserId, url, timeoutMs } = {}) {
        const browser = this.#getBrowser(browserId);
        const page = await browser.context.newPage();
        const record = this.#addPage(browser, page);
        if (url) await page.goto(String(url), { timeout: timeoutMs });
        return { pageId: record.id, generation: record.generation };
    }

    async pageClose({ pageId } = {}) {
        const record = this.#getPage(pageId);
        try { await record.page.close(); } finally { this.#forgetPage(pageId, 'closed-by-client'); }
        return { ok: true };
    }

    async pageCall({ pageId, operations } = {}) {
        const record = this.#getPage(pageId);
        if (!Array.isArray(operations)) throw new Error('operations must be an array');
        const results = [];
        for (let index = 0; index < operations.length; index++) {
            try { results.push(await this.#operation(record, operations[index])); }
            catch (error) { return { results, failedIndex: index, error: errorMessage(error) }; }
        }
        return { results, failedIndex: null };
    }

    async #operation(record, op) {
        if (!op || typeof op !== 'object' || typeof op.op !== 'string') throw new Error('each operation requires op');
        const page = record.page;
        switch (op.op) {
            case 'goto': {
                const response = await page.goto(String(op.url), op.options || {});
                return response ? { url: response.url(), status: response.status() } : null;
            }
            case 'waitForLoadState': return await page.waitForLoadState(op.state || 'load', op.options || {});
            case 'waitForTimeout': return await page.waitForTimeout(Number(op.ms) || 0);
            case 'waitForURL': return await page.waitForURL(String(op.url), op.options || {});
            case 'url': return page.url();
            case 'title': return await page.title();
            case 'locator.count': return await this.#locator(page, op.locator).count();
            case 'locator.waitFor': return await this.#locator(page, op.locator).waitFor(op.options || {});
            case 'locator.visible': return await this.#locator(page, op.locator).isVisible();
            case 'locator.enabled': return await this.#locator(page, op.locator).isEnabled();
            case 'locator.connected': return await this.#locator(page, op.locator).evaluate(element => element.isConnected);
            case 'locator.boundingBox': return await this.#locator(page, op.locator).boundingBox(op.options || {});
            case 'locator.scrollIntoViewIfNeeded': return await this.#locator(page, op.locator).scrollIntoViewIfNeeded(op.options || {});
            case 'shadow.query': return await this.#shadowQuery(record, op);
            case 'frame.fromElement': return await this.#frameFromElement(record, op);
            case 'frame.click': return await this.#frameClick(record, op);
            case 'element.boundingBox': return await this.#element(record, op.elementId).handle.boundingBox();
            case 'locator.text': return await this.#locator(page, op.locator).innerText(op.options || {});
            case 'locator.attribute': return await this.#locator(page, op.locator).getAttribute(String(op.name), op.options || {});
            case 'locator.remove': return await this.#locator(page, op.locator).evaluateAll(elements => {
                for (const element of elements) element.remove();
                return elements.length;
            });
            case 'locator.setStyle': return await this.#locator(page, op.locator).evaluateAll((elements, styles) => {
                for (const element of elements) for (const [key, value] of Object.entries(styles)) element.style.setProperty(key, String(value));
                return elements.length;
            }, op.styles || {});
            case 'locator.click': return await this.#locator(page, op.locator).click(op.options || {});
            case 'locator.fill': return await this.#locator(page, op.locator).fill(String(op.value ?? ''), op.options || {});
            case 'locator.press': return await this.#locator(page, op.locator).press(String(op.key), op.options || {});
            case 'keyboard.press': return await page.keyboard.press(String(op.key));
            case 'keyboard.type': return await page.keyboard.type(String(op.text ?? ''), op.options || {});
            case 'mouse.click': return await page.mouse.click(Number(op.x), Number(op.y), op.options || {});
            case 'mouse.move': return await page.mouse.move(Number(op.x), Number(op.y), op.options || {});
            case 'mouse.wheel': return await page.mouse.wheel(Number(op.deltaX || 0), Number(op.deltaY || 0));
            case 'upload': return await this.#locator(page, op.locator).setInputFiles(op.files, op.options || {});
            case 'filechooser.wait': {
                const chooser = await page.waitForEvent('filechooser', op.options || {});
                const id = opaqueId();
                this.fileChoosers ||= new Map();
                this.fileChoosers.set(id, chooser);
                return { chooserId: id, multiple: chooser.isMultiple?.() ?? false };
            }
            case 'filechooser.setFiles': {
                const chooser = this.fileChoosers?.get(op.chooserId);
                if (!chooser) throw new Error('unknown chooserId');
                await chooser.setFiles(op.files, op.options || {});
                this.fileChoosers.delete(op.chooserId);
                return { ok: true };
            }
            case 'filechooser.clickAndSetFiles': {
                const [chooser] = await Promise.all([
                    page.waitForEvent('filechooser', op.options || {}),
                    this.#locator(page, op.locator).click(op.clickOptions || {})
                ]);
                await chooser.setFiles(op.files, op.fileOptions || {});
                return { ok: true };
            }
            case 'screenshot': {
                if (!op.path) throw new Error('screenshot requires path');
                await fs.promises.mkdir(path.dirname(op.path), { recursive: true });
                await page.screenshot({ ...(op.options || {}), path: op.path });
                return { path: op.path };
            }
            case 'evaluate': return await this.#evaluate(page, op);
            default: throw new Error(`unsupported operation: ${op.op}`);
        }
    }

    #locator(page, desc) {
        if (!desc || typeof desc !== 'object') throw new Error('locator descriptor required');
        let locator;
        switch (desc.kind) {
            case 'css': locator = page.locator(String(desc.value)); break;
            case 'role': locator = page.getByRole(String(desc.role), { name: desc.name, exact: desc.exact }); break;
            case 'text': locator = page.getByText(String(desc.value), { exact: desc.exact }); break;
            default: throw new Error(`unsupported locator kind: ${desc.kind}`);
        }
        for (const step of desc.chain || []) {
            switch (step.op) {
                case 'first': locator = locator.first(); break;
                case 'last': locator = locator.last(); break;
                case 'nth': locator = locator.nth(Number(step.index)); break;
                case 'filter': locator = locator.filter({ hasText: step.hasText, hasNotText: step.hasNotText, visible: step.visible }); break;
                case 'locator': locator = locator.locator(String(step.selector)); break;
                default: throw new Error(`unsupported locator chain operation: ${step.op}`);
            }
        }
        return locator;
    }

    #element(record, elementId) {
        const element = this.elements.get(elementId);
        if (!element || element.pageId !== record.id) throw new Error('unknown or expired elementId');
        return element;
    }

    async #shadowQuery(record, op) {
        const root = op.root || { kind: 'page' };
        let target = record.page;
        let frameId = null;
        if (root.kind === 'frame') {
            const found = this.frames.get(root.frameId);
            if (!found || found.pageId !== record.id) throw new Error('unknown or expired frameId');
            target = found.frame;
            frameId = root.frameId;
        } else if (root.kind !== 'page') throw new Error('unsupported shadow root kind');
        const property = op.shadowProperty || 'shadowRootUnl';
        if (!['shadowRootUnl', 'shadowRoot'].includes(property)) throw new Error('unsupported shadow property');
        const handle = await target.evaluateHandle(({ hostSelector, property, selector, includeRoot }) => {
            const base = hostSelector ? document.querySelector(hostSelector) : document.body;
            if (!base) return null;
            const candidates = includeRoot ? [base, ...base.querySelectorAll('*')] : [...base.querySelectorAll('*')];
            const host = candidates.find(node => node[property]);
            return host?.[property]?.querySelector(selector) || null;
        }, { hostSelector: op.hostSelector || null, property, selector: String(op.selector), includeRoot: op.includeRoot !== false });
        const element = handle.asElement?.();
        if (!element) { await handle.dispose?.(); return null; }
        const elementId = opaqueId();
        this.elements.set(elementId, { pageId: record.id, frameId, handle: element });
        return { elementId, boundingBox: await element.boundingBox().catch(() => null) };
    }

    async #frameFromElement(record, op) {
        const element = this.#element(record, op.elementId);
        const frame = await element.handle.contentFrame();
        if (!frame) return { frameId: null };
        const frameId = opaqueId();
        this.frames.set(frameId, { pageId: record.id, frame });
        return { frameId };
    }

    async #frameClick(record, op) {
        const element = this.#element(record, op.elementId);
        if (element.frameId !== op.frameId || !this.frames.has(op.frameId)) throw new Error('element is not in frameId');
        await element.handle.click(op.options || {});
        return { ok: true };
    }

    async #evaluate(page, { expression, args = null } = {}) {
        const scripts = {
            'document.title': () => document.title,
            'document.url': () => location.href,
            'document.bodyText': () => document.body?.innerText ?? '',
            'document.html': () => document.documentElement?.outerHTML ?? '',
            'element.text': ({ selector }) => document.querySelector(selector)?.textContent ?? null,
            'element.attribute': ({ selector, name }) => document.querySelector(selector)?.getAttribute(name) ?? null,
            'element.exists': ({ selector }) => document.querySelector(selector) !== null,
            'element.click': ({ selector }) => { const e = document.querySelector(selector); if (!e) return false; e.click(); return true; },
            'element.setValue': ({ selector, value }) => { const e = document.querySelector(selector); if (!e) return false; e.value = value; e.dispatchEvent(new Event('input', { bubbles: true })); e.dispatchEvent(new Event('change', { bubbles: true })); return true; }
        };
        const script = scripts[expression];
        if (!script) throw new Error(`unsupported fixed evaluate expression: ${expression}`);
        return await page.evaluate(script, args);
    }

    #captureResponse(pageRecord, response) {
        const id = opaqueId();
        this.responses.set(id, response);
        // Retain only recent handles; body retrieval is an explicit RPC.
        if (this.responses.size > 2048) this.responses.delete(this.responses.keys().next().value);
        let url; let status; let method; let headers;
        try { url = response.url(); status = response.status(); method = response.request().method(); headers = response.headers(); } catch {}
        this.#emit({ type: 'response', browserId: pageRecord.browserId, pageId: pageRecord.id, generation: pageRecord.generation, responseId: id, url, status, method, headers });
    }

    async eventSubscribe({ browserId, pageId, types = ['*'] } = {}) {
        if (browserId) this.#getBrowser(browserId);
        if (pageId) this.#getPage(pageId);
        const id = opaqueId();
        this.subscriptions.set(id, { id, browserId: browserId || null, pageId: pageId || null, types: new Set(types), cursor: this.eventSeq });
        return { subscriptionId: id, afterSequence: this.eventSeq };
    }

    eventUnsubscribe({ subscriptionId } = {}) {
        return { ok: this.subscriptions.delete(subscriptionId) };
    }

    eventPoll({ afterSequence = 0, limit = 256 } = {}) {
        const first = this.events[0]?.sequence ?? this.eventSeq + 1;
        const count = Math.min(1024, Math.max(1, Number(limit) || 256));
        return {
            events: this.events.filter(event => event.sequence > afterSequence).slice(0, count),
            dropped: afterSequence < first - 1 ? first - afterSequence - 1 : 0,
            latestSequence: this.eventSeq
        };
    }

    async responseBody({ responseId, path: outputPath } = {}) {
        const response = this.responses.get(responseId);
        if (!response) throw new Error('unknown or expired responseId');
        const body = await response.body();
        if (outputPath) {
            await fs.promises.mkdir(path.dirname(outputPath), { recursive: true });
            await fs.promises.writeFile(outputPath, body);
            return { path: outputPath, bytes: body.length };
        }
        return { base64: Buffer.from(body).toString('base64'), bytes: body.length };
    }

    async responseWaitFinished({ responseId, timeoutMs = 60000 } = {}) {
        const response = this.responses.get(responseId);
        if (!response) throw new Error('unknown or expired responseId');
        const deadline = Math.min(300000, Math.max(1, Number(timeoutMs) || 60000));
        let timer;
        try {
            const completion = typeof response.finished === 'function' ? response.finished() : response.body();
            const result = await Promise.race([
                completion,
                new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('response finish timeout')), deadline); })
            ]);
            if (result instanceof Error) throw result;
            return { ok: true };
        } finally {
            clearTimeout(timer);
        }
    }

    async routeInstall({ pageId, pattern = '**/*', timeoutMs } = {}) {
        const page = this.#getPage(pageId);
        const id = opaqueId();
        const timeout = Math.min(MAX_ROUTE_TIMEOUT_MS, Math.max(25, Number(timeoutMs) || this.routeTimeoutMs));
        const routeHandler = async route => {
            const token = opaqueId();
            const req = route.request();
            const entry = { token, route, timer: null, settled: false };
            this.routes.set(token, entry);
            const meta = { type: 'route', browserId: page.browserId, pageId, generation: page.generation, routeToken: token, request: { url: req.url(), method: req.method(), headers: req.headers(), resourceType: req.resourceType(), postData: req.postData() } };
            this.#emit(meta);
            entry.timer = setTimeout(() => this.#settleRoute(entry, { action: 'continue' }).catch(() => {}), timeout);
        };
        await page.page.route(pattern, routeHandler);
        const remove = () => Promise.resolve(page.page.unroute(pattern, routeHandler)).catch(() => {});
        page.hooks.push(remove);
        this.routes.set(`install:${id}`, { id, pageId, pattern, handler: routeHandler, remove, timeout });
        return { routeId: id, pattern, timeoutMs: timeout };
    }

    async routeResolve({ routeToken, action = 'continue', ...options } = {}) {
        const entry = this.routes.get(routeToken);
        if (!entry || routeToken.startsWith('install:')) throw new Error('unknown or expired routeToken');
        await this.#settleRoute(entry, { action, ...options });
        return { ok: true };
    }

    async #settleRoute(entry, decision) {
        if (entry.settled) return;
        entry.settled = true;
        clearTimeout(entry.timer);
        this.routes.delete(entry.token);
        if (decision.action === 'abort') await entry.route.abort(decision.errorCode || 'failed');
        else if (decision.action === 'fulfill') await entry.route.fulfill({ status: decision.status, headers: decision.headers, body: decision.body, contentType: decision.contentType });
        else if (decision.action === 'continue') await entry.route.continue({ url: decision.url, method: decision.method, headers: decision.headers, postData: decision.postData });
        else throw new Error(`unsupported route action: ${decision.action}`);
    }

    async routeRemove({ routeId } = {}) {
        const entry = this.routes.get(`install:${routeId}`);
        if (!entry) return { ok: false };
        this.routes.delete(`install:${routeId}`);
        await entry.remove();
        return { ok: true };
    }

    async downloadFetch({ pageId, url, path: outputPath, headers = {}, timeoutMs } = {}) {
        const page = this.#getPage(pageId);
        if (!outputPath) throw new Error('download.fetch requires path');
        const response = await page.page.request.get(String(url), { headers, timeout: timeoutMs });
        if (!response.ok()) throw new Error(`download failed with HTTP ${response.status()}`);
        const body = await response.body();
        await fs.promises.mkdir(path.dirname(outputPath), { recursive: true });
        await fs.promises.writeFile(outputPath, body);
        this.downloads.add(outputPath);
        this.#emit({ type: 'download.completed', browserId: page.browserId, pageId, path: outputPath, bytes: body.length, status: response.status() });
        return { path: outputPath, bytes: body.length, status: response.status(), headers: response.headers() };
    }

    async cookiesGet({ browserId, urls = [] } = {}) {
        const browser = this.#getBrowser(browserId);
        return { cookies: await browser.context.cookies(urls) };
    }

    async shutdown() {
        if (this.closed) return { ok: true };
        this.closed = true;
        for (const entry of [...this.routes.values()]) {
            if (entry.token) await this.#settleRoute(entry, { action: 'continue' }).catch(() => {});
            else await entry.remove?.();
        }
        for (const browserId of [...this.browsers.keys()]) await this.browserClose({ browserId }).catch(() => {});
        try { await (await this.#launcher()).cleanup?.(); } catch {}
        this.subscriptions.clear();
        this.responses.clear();
        const server = this.server;
        const socket = this.socket;
        // Let the dispatch promise write the shutdown response before closing
        // the transport used to carry it.
        setImmediate(() => {
            if (server) server.close(() => { if (this.socketPath) { try { fs.unlinkSync(this.socketPath); } catch {} } });
            else if (this.socketPath) { try { fs.unlinkSync(this.socketPath); } catch {} }
            socket?.end();
        });
        return { ok: true };
    }

    #getBrowser(id) { const item = this.browsers.get(id); if (!item || item.closed) throw new Error(`unknown or closed browserId: ${id}`); return item; }
    #getPage(id) { const item = this.pages.get(id); if (!item || item.closed) throw new Error(`unknown or closed pageId: ${id}`); return item; }

    #emit(event) {
        const envelope = { event: 'runtime', sequence: ++this.eventSeq, ...event };
        if (this.events.length >= this.eventBufferLimit) { this.events.shift(); this.droppedEvents++; }
        this.events.push(envelope);
        if (this.socket && !this.socket.destroyed) {
            for (const sub of this.subscriptions.values()) {
                if (sub.browserId && sub.browserId !== event.browserId) continue;
                if (sub.pageId && sub.pageId !== event.pageId) continue;
                const type = event.type || '';
                if (!sub.types.has('*') && !sub.types.has(type)) continue;
                this.socket.write(`${JSON.stringify(envelope)}\n`);
            }
        }
    }

    async dispatch(request) {
        const handler = this.handlers[request?.method];
        if (!handler) throw new Error(`unknown method: ${request?.method}`);
        return await handler(request.params || {});
    }

    listen(socketPath) {
        if (!socketPath) throw new Error('socket path is required');
        try { fs.unlinkSync(socketPath); } catch (e) { if (e.code !== 'ENOENT') throw e; }
        this.socketPath = socketPath;
        this.server = net.createServer(socket => this.#accept(socket));
        this.server.listen(socketPath, () => { try { fs.chmodSync(socketPath, 0o600); } catch {} });
        return this.server;
    }

    #accept(socket) {
        if (this.socket && !this.socket.destroyed) { socket.end(); return; }
        this.socket = socket;
        socket.on('close', () => {
            // A disconnected Rust peer cannot resolve pending routes. Release
            // every intercepted request immediately using the safe default.
            for (const entry of [...this.routes.values()]) if (entry.token) this.#settleRoute(entry, { action: 'continue' }).catch(() => {});
            this.subscriptions.clear();
        });
        socket.setEncoding('utf8');
        let buffer = '';
        socket.write(`${JSON.stringify({ event: 'ready' })}\n`);
        socket.on('data', chunk => {
            buffer += chunk;
            if (Buffer.byteLength(buffer) > MAX_LINE_BYTES && !buffer.includes('\n')) { socket.destroy(new Error('request frame too large')); return; }
            let index;
            while ((index = buffer.indexOf('\n')) >= 0) {
                const line = buffer.slice(0, index).trim(); buffer = buffer.slice(index + 1);
                if (!line) continue;
                if (Buffer.byteLength(line) > MAX_LINE_BYTES) { this.#reply(null, null, new Error('request frame too large')); continue; }
                let req;
                try { req = JSON.parse(line); } catch { this.#reply(null, null, new Error('invalid JSON')); continue; }
                Promise.resolve(this.dispatch(req)).then(result => this.#reply(req.id ?? null, result), error => this.#reply(req.id ?? null, null, error));
            }
        });
        socket.on('close', () => { if (this.socket === socket) this.socket = null; });
    }

    #reply(id, result, error) {
        if (!this.socket || this.socket.destroyed) return;
        const response = error ? { id, error: errorMessage(error) } : { id, result };
        this.socket.write(`${JSON.stringify(response)}\n`);
    }
}

export async function startRuntime(socketPath = process.env.WEBAI2API_SOCK) {
    const runtime = new BrowserRuntime();
    runtime.listen(socketPath);
    return runtime;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
    if (!process.env.WEBAI2API_SOCK) { console.error('WEBAI2API_SOCK is required'); process.exit(1); }
    const runtime = await startRuntime();
    const stop = async () => { await runtime.shutdown(); process.exit(0); };
    process.once('SIGINT', stop);
    process.once('SIGTERM', stop);
}
