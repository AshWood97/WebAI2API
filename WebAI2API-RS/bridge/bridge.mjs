/**
 * 引擎桥进程：把原仓库的浏览器驱动层（adapter/engine/pool）暴露为 JSON 行协议，
 * 供 Rust 核心通过 Unix socket 调用。本文件不复制任何浏览器逻辑，全部 import 原仓库源码。
 *
 * 环境变量：
 *   WEBAI2API_SRC_ROOT  原仓库根目录（含 src/、node_modules/、camoufox/）
 *   WEBAI2API_SOCK      Unix socket 路径
 *   WEBAI2API_LOGIN     登录模式："" | "1" | "<workerName>"
 *   WEBAI2API_TEMP_DIR  生成图片临时目录（默认 backend.TEMP_DIR，即 src-root/data/temp）
 *
 * 协议：每行一个 JSON。请求 {id, method, params} → 响应 {id, result} | {id, error}。
 * 服务端事件（无 id）：{event:"log"|"fatal", ...}。图片结果走文件路径，不经 socket。
 */

import net from 'net';
import fs from 'fs';
import path from 'path';
import { pathToFileURL } from 'url';

const SRC_ROOT = process.env.WEBAI2API_SRC_ROOT;
const SOCK = process.env.WEBAI2API_SOCK;
if (!SRC_ROOT || !SOCK) {
    console.error('bridge: 缺少 WEBAI2API_SRC_ROOT 或 WEBAI2API_SOCK');
    process.exit(1);
}

// camoufox-js 在模块加载时读取该变量，必须先于任何 backend import
if (!process.env.CAMOUFOX_INSTALL_DIR) {
    process.env.CAMOUFOX_INSTALL_DIR = path.join(SRC_ROOT, 'camoufox');
}

const src = (...parts) => pathToFileURL(path.join(SRC_ROOT, 'src', ...parts)).href;

const { getBackend } = await import(src('backend', 'index.js'));
const { registry } = await import(src('backend', 'registry.js'));
const { logger } = await import(src('utils', 'logger.js'));
const { useContextDownload } = await import(src('backend', 'utils', 'download.js'));

let backend = null;
let poolContext = null;

// 把 JS 日志转发回 Rust 侧统一落盘。
// WEBAI2API_BRIDGE_QUIET=1 时抑制 logger.js 自身的 console 输出，
// 避免与 Rust 侧重打印形成重复行（日志以 Rust 侧为唯一出口）。
const QUIET = process.env.WEBAI2API_BRIDGE_QUIET === '1';
for (const level of ['debug', 'info', 'warn', 'error']) {
    const original = logger[level]?.bind(logger);
    if (!original) continue;
    logger[level] = (module, message, meta) => {
        if (!QUIET) original(module, message, meta);
        emit({
            event: 'log',
            level,
            module: String(module ?? ''),
            message: String(message ?? ''),
            meta: meta && typeof meta === 'object' ? stringifyMeta(meta) : {}
        });
    };
}

function stringifyMeta(meta) {
    const out = {};
    for (const [k, v] of Object.entries(meta)) {
        if (v instanceof Error) out[k] = v.message;
        else if (v === null || ['string', 'number', 'boolean'].includes(typeof v)) out[k] = v;
        else { try { out[k] = JSON.stringify(v); } catch { out[k] = String(v); } }
    }
    return out;
}

let socket = null;
function emit(obj) {
    if (socket && !socket.destroyed) socket.write(JSON.stringify(obj) + '\n');
}

/**
 * 把生成结果里的 data URI 图片落成临时文件，避免大 payload 过 socket。
 * 返回 {text?, imagePath?, imageUrl?, reasoning?, error?, code?, retryable?}
 */
function normalizeResult(result) {
    if (!result || typeof result !== 'object') return { error: '适配器返回空结果' };
    const out = {};
    if (result.error) {
        out.error = String(result.error);
        if (result.code) out.code = String(result.code);
        if (typeof result.retryable === 'boolean') out.retryable = result.retryable;
        return out;
    }
    if (typeof result.text === 'string') out.text = result.text;
    if (typeof result.reasoning === 'string') out.reasoning = result.reasoning;
    if (typeof result.imageUrl === 'string') out.imageUrl = result.imageUrl;
    if (typeof result.image === 'string' && result.image.startsWith('data:')) {
        const match = result.image.match(/^data:[^;]+;base64,(.+)$/);
        if (match) {
            const tempDir = process.env.WEBAI2API_TEMP_DIR || backend.TEMP_DIR;
            const file = path.join(tempDir, `bridge_${Date.now()}_${Math.random().toString(36).slice(2, 8)}.b64img`);
            fs.mkdirSync(path.dirname(file), { recursive: true });
            fs.writeFileSync(file, Buffer.from(match[1], 'base64'));
            out.imagePath = file;
            out.imageMime = (result.image.match(/^data:([^;]+);/) || [])[1] || 'image/png';
        }
    }
    return out;
}

/**
 * 脱敏 runtime/状态中的路径与代理信息（对应原版 sanitizeRuntimeForApi）
 * @param {object} runtime
 * @returns {object}
 */
function sanitizeRuntime(runtime) {
    if (!runtime || typeof runtime !== 'object' || Array.isArray(runtime)) {
        return runtime;
    }
    const out = { ...runtime };
    if (out.binaryPath) out.binaryPath = path.basename(String(out.binaryPath));
    if (out.userDataDir) out.userDataDir = path.basename(String(out.userDataDir));
    if (out.runtime && typeof out.runtime === 'object') {
        out.runtime = sanitizeRuntime(out.runtime);
    }
    if (out.workers && Array.isArray(out.workers)) {
        out.workers = out.workers.map(w => sanitizeRuntime(w));
    }
    delete out.proxyPassword;
    delete out.proxyUsername;
    delete out.__meta;
    return out;
}

/**
 * 把 Worker 序列化为 API 视图（对应原版 sanitizeRuntimeForApi 之后的 worker 形状）
 * @param {object} w
 * @param {object} pm
 */
function serializeWorker(w, pm) {
    return {
        name: w.name,
        instance: w.instanceName || w.name,
        type: w.type || w.adapterType || null,
        adapter: w.type || null,
        engine: w.engine,
        // 对齐原版 sanitizeRuntimeForApi：userDataDir 只暴露目录名
        userDataDir: w.userDataDir ? path.basename(String(w.userDataDir)) : w.userDataDir,
        busy: (w.busyCount || 0) > 0,
        busyCount: w.busyCount || 0,
        pageReady: !!w.page,
        // 浏览器是否已初始化可视为“登录态可用”的粗粒度信号
        authReady: !!w.page || !!pm,
        mergeTypes: w.mergeTypes || [],
        stopped: w.initialized === false,
        runtime: w.runtime ? sanitizeRuntime(w.runtime) : null
    };
}

function listAdapters() {
    return registry.getAdapterIds().map((id) => {
        const adapter = registry.getAdapter(id);
        return {
            id,
            displayName: adapter.displayName || id,
            description: adapter.description || '',
            configSchema: adapter.configSchema || [],
            models: (adapter.models || []).map(m => ({
                id: m.id,
                codeName: m.codeName || null,
                imagePolicy: m.imagePolicy || 'optional',
                type: m.type || null
            }))
        };
    });
}

const methods = {
    // 启动预检（对齐 server.js runPreflight）：依赖损坏/内核缺失时抛错，Rust 侧致命退出
    async preflight() {
        const { runPreflight } = await import(src('server', 'preflight.js'));
        await runPreflight();
        return { ok: true };
    },

    async init(params) {
        const login = process.env.WEBAI2API_LOGIN || '';
        if (login) {
            // PoolManager.initAll 从 process.argv 解析 -login[=worker]
            const arg = login === '1' ? '-login' : `-login=${login}`;
            if (!process.argv.includes(arg)) process.argv.push(arg);
        }
        backend = getBackend();
        poolContext = await backend.initBrowser(params?.config || backend.config);
        const pm = poolContext.poolManager;
        const workers = pm.workers.map(w => serializeWorker(w, pm));
        return {
            ok: true,
            workers,
            browserCount: new Set(pm.workers.map(w => `${w.engine}::${w.userDataDir}`)).size,
            browserStopped: typeof pm.isBrowserStopped === 'function' ? !!pm.isBrowserStopped() : false
        };
    },

    async generate(params) {
        const { prompt, imagePaths, modelId, id, reasoning } = params || {};
        const result = await backend.generate(poolContext, prompt, imagePaths || [], modelId, { id, reasoning });
        return normalizeResult(result);
    },

    async getModels() {
        return backend.getModels();
    },

    // 聚合查询：一次 IPC 返回模型表 + 每模型的 type/imagePolicy，
    // 供 Rust 侧缓存，避免每个聊天请求 3 次串行往返。
    async modelInfo(params) {
        const models = backend.getModels();
        const wantType = params?.model ? String(params.model) : null;
        const data = Array.isArray(models?.data) ? models.data : [];
        let modelType = null;
        let imagePolicy = null;
        if (wantType) {
            for (const m of data) {
                if (m.id === wantType) {
                    modelType = m.type || null;
                    imagePolicy = m.imagePolicy || m.image_policy || 'optional';
                    break;
                }
            }
            // 模型不在表内时回退到单点查询（与 Node 对未知模型的处理一致）
            if (modelType === null) {
                modelType = backend.getModelType(wantType) || 'image';
                imagePolicy = backend.getImagePolicy(wantType) || 'optional';
            }
        }
        return { models, modelType, imagePolicy };
    },

    async getImagePolicy(params) {
        return { policy: backend.getImagePolicy(params?.model) };
    },

    async getModelType(params) {
        return { type: backend.getModelType(params?.model) };
    },

    async getCookies(params) {
        const { instance, domain } = params || {};
        const result = await backend.getCookies(instance || undefined, domain || undefined);
        // 对齐原版 handleCookies 的响应构造：worker 字段在 backend 返回 instance 时为 undefined，
        // JSON 序列化后只剩 {cookies}
        return { worker: result.worker, cookies: result.cookies };
    },

    async navigateToMonitor() {
        await backend.navigateToMonitor();
        return { ok: true };
    },

    async listAdapters() {
        return { adapters: listAdapters() };
    },

    async downloadViaContext(params) {
        const page = poolContext?.poolManager?.getFirstPage?.();
        if (!page) return { error: '浏览器未初始化' };
        const retries = Number(params?.retries) > 0 ? Number(params.retries) : 1;
        const result = await useContextDownload(params.url, page, { retries });
        if (!result || result.error) return { error: result?.error || '下载失败' };
        if (typeof result.image === 'string' && result.image.startsWith('data:')) {
            const normalized = normalizeResult(result);
            return { path: normalized.imagePath, mime: normalized.imageMime };
        }
        return { error: '下载结果不是图片' };
    },

    async restartBrowser() {
        const pm = poolContext?.poolManager;
        if (!pm || typeof pm.restartBrowser !== 'function') {
            return { browserStopped: false, error: '工作池未初始化（可能处于安全模式），请先通过 WebUI 重启服务' };
        }
        const result = await pm.restartBrowser();
        return {
            browserStopped: typeof pm.isBrowserStopped === 'function' ? !!pm.isBrowserStopped() : false,
            workers: pm.workers.map(w => ({ ...serializeWorker(w, pm) })),
            result
        };
    },

    async shutdown() {
        try {
            const { cleanup } = await import(src('backend', 'engine', 'launcher.js'));
            await cleanup();
        } catch (e) {
            emit({ event: 'log', level: 'warn', module: '桥接', message: `清理失败: ${e.message}`, meta: {} });
        }
        return { ok: true };
    }
};

// ==================== 连接 ====================

function handleConnection(sock) {
    socket = sock;
    sock.setEncoding('utf8');
    let buffer = '';
    sock.on('data', (chunk) => {
        buffer += chunk;
        let idx;
        while ((idx = buffer.indexOf('\n')) >= 0) {
            const line = buffer.slice(0, idx).trim();
            buffer = buffer.slice(idx + 1);
            if (line) dispatch(line);
        }
    });
    sock.on('error', (err) => {
        console.error(`bridge: socket 错误: ${err.message}`);
    });
    sock.on('close', () => {
        socket = null;
        // Rust 侧断开即视为服务停止
        process.exit(0);
    });
    emit({ event: 'ready' });
}

async function dispatch(line) {
    let req;
    try {
        req = JSON.parse(line);
    } catch {
        emit({ id: null, error: '请求不是合法 JSON' });
        return;
    }
    const handler = methods[req.method];
    if (!handler) {
        emit({ id: req.id ?? null, error: `未知方法: ${req.method}` });
        return;
    }
    try {
        const result = await handler(req.params || {});
        emit({ id: req.id ?? null, result });
    } catch (e) {
        emit({ id: req.id ?? null, error: e?.message || String(e) });
    }
}

if (fs.existsSync(SOCK)) fs.unlinkSync(SOCK);
const server = net.createServer(handleConnection);
server.listen(SOCK, () => {
    try { fs.chmodSync(SOCK, 0o600); } catch { /* Windows 无 chmod */ }
    console.error(`bridge: 监听 ${SOCK}`);
});
server.on('error', (err) => {
    console.error(`bridge: 启动失败: ${err.message}`);
    process.exit(1);
});

process.on('SIGTERM', () => process.exit(0));
process.on('SIGINT', () => process.exit(0));
