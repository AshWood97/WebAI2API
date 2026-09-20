/**
 * @fileoverview OpenAI 兼容 API 路由
 * @description 处理 /v1 路径下的所有 API 请求
 * 升级点（借鉴 WebAI-to-API / WebModel）：
 * - Stateless 端点
 * - Runtime / Auth / Providers 状态 API
 * - OpenAPI schema
 */

import crypto from 'crypto';
import fs from 'fs';
import path from 'path';
import { logger } from '../../../utils/logger.js';
import { ERROR_CODES } from '../../errors.js';
import { sendJson, sendApiError } from '../../respond.js';
import { parseRequest } from './parse.js';
import { buildOpenApiSchema } from './openapi.js';
import { getSystemStatus } from '../../../utils/systemInfo.js';
import { getTodayStats } from '../../../utils/stats.js';
import { readCamoufoxVersion } from '../../../backend/engine/camoufoxMeta.js';
import { PROJECT_CAMOUFOX_DIR } from '../../../backend/engine/camoufoxEnv.js';

/**
 * 创建 OpenAI API 路由处理器
 * @param {object} context - 路由上下文
 * @returns {Function} 路由处理函数
 */
export function createOpenAIRouter(context) {
    const {
        backendName,
        getModels,
        getImagePolicy,
        getModelType,
        tempDir,
        imageLimit,
        queueManager,
        config,
        loginMode,
        getSafeMode
    } = context;

    /**
     * 处理 GET /v1/models
     */
    function handleModels(res) {
        const models = getModels();
        sendJson(res, 200, models);
    }

    /**
     * 处理 GET /v1/openapi.json
     */
    function handleOpenApi(res) {
        const models = getModels();
        let version = '3.0.0';
        try {
            const pkg = JSON.parse(fs.readFileSync(path.join(process.cwd(), 'package.json'), 'utf8'));
            version = pkg.version || version;
        } catch { /* ignore */ }
        sendJson(res, 200, buildOpenApiSchema({ models, version }));
    }

    /**
     * 处理 GET /v1/runtime/status — 借鉴 WebAI-to-API
     */
    function handleRuntimeStatus(res) {
        const system = getSystemStatus();
        const queue = queueManager.getStatus();
        const detailed = queueManager.getDetailedStatus();
        const safeMode = getSafeMode?.() || { enabled: false, reason: null };
        const poolContext = queueManager.getPoolContext();
        const models = getModels();
        const todayStats = getTodayStats();

        sendJson(res, 200, {
            service: 'webai2api',
            backend: backendName,
            version: system.version,
            uptime: system.uptime,
            status: safeMode.enabled ? 'safe_mode' : (loginMode ? 'login_mode' : (poolContext?.poolManager ? 'ready' : 'starting')),
            safeMode,
            loginMode: !!loginMode,
            pool: {
                ready: !!poolContext?.poolManager,
                workerCount: poolContext?.poolManager?.workers?.length || 0,
                strategy: config?.backend?.pool?.strategy || 'least_busy'
            },
            queue: {
                processing: queue.processing,
                waiting: queue.queueLength,
                total: queue.total,
                maxConcurrent: config?.queue?.maxConcurrent || 1,
                processingTasks: detailed.processing,
                waitingTasks: detailed.waiting
            },
            models: {
                count: models.data?.length || 0,
                text: (models.data || []).filter(m => m.type === 'text').length,
                image: (models.data || []).filter(m => m.type !== 'text').length
            },
            stats: {
                todaySuccess: todayStats.success || 0,
                todayFailed: todayStats.failed || 0
            },
            system: {
                status: system.status,
                cpuUsage: system.cpuUsage,
                memoryUsage: system.memoryUsage,
                systemVersion: system.systemVersion
            },
            camoufox: readCamoufoxVersion(process.env.CAMOUFOX_INSTALL_DIR || PROJECT_CAMOUFOX_DIR),
            keepaliveMode: config?.server?.keepalive?.mode || 'comment',
            timestamp: new Date().toISOString()
        });
    }

    /**
     * 处理 GET /v1/auth/status — 借鉴 WebAI-to-API / WebModel
     */
    function handleAuthStatus(res) {
        const poolContext = queueManager.getPoolContext();
        const workers = poolContext?.poolManager?.workers || [];
        const instances = config?.backend?.pool?.instances || [];

        const workerStatus = workers.map(w => ({
            name: w.name,
            instance: w.instanceName || w.name,
            type: w.type || w.adapterType,
            adapter: w.type || null,
            busy: (w.busyCount || 0) > 0,
            busyCount: w.busyCount || 0,
            pageReady: !!w.page,
            // 浏览器是否已初始化可视为“登录态可用”的粗粒度信号
            authReady: !!w.page || !!poolContext?.poolManager,
            mergeTypes: w.mergeTypes || null
        }));

        // 配置了但未运行的实例
        const runningNames = new Set(workers.map(w => w.name));
        const configuredOnly = (instances || [])
            .flatMap(inst => (inst.workers || []).map(w => ({
                name: w.name,
                instance: inst.name,
                type: w.type,
                authReady: false,
                pageReady: false,
                busy: false,
                status: 'not_running'
            })))
            .filter(w => !runningNames.has(w.name));

        const all = [...workerStatus, ...configuredOnly];
        const readyCount = all.filter(w => w.authReady).length;

        sendJson(res, 200, {
            object: 'auth.status',
            backend: backendName,
            poolReady: !!poolContext?.poolManager,
            loginMode: !!loginMode,
            readyCount,
            totalCount: all.length,
            overall: poolContext?.poolManager && readyCount > 0 ? 'authenticated' : 'unknown',
            workers: all,
            note: 'authReady 为粗粒度信号：浏览器实例已初始化即视为可用。精确登录态需通过 /v1/cookies 或 WebUI 人工确认。',
            timestamp: new Date().toISOString()
        });
    }

    /**
     * 处理 GET /v1/providers — 借鉴 WebModel /webmodel/providers
     */
    function handleProviders(res) {
        const models = getModels();
        const poolContext = queueManager.getPoolContext();
        const workers = poolContext?.poolManager?.workers || [];

        // 按 adapter/owned_by 聚合模型
        const byAdapter = new Map();
        for (const m of models.data || []) {
            const owner = m.owned_by || 'unknown';
            if (!byAdapter.has(owner)) {
                byAdapter.set(owner, {
                    id: owner,
                    name: owner,
                    models: [],
                    modelCount: 0,
                    textCount: 0,
                    imageCount: 0,
                    workers: [],
                    enabled: true
                });
            }
            const bucket = byAdapter.get(owner);
            bucket.models.push({
                id: m.id,
                type: m.type || 'image',
                image_policy: m.image_policy
            });
            bucket.modelCount++;
            if (m.type === 'text') bucket.textCount++;
            else bucket.imageCount++;
        }

        // 关联运行中的 worker
        for (const w of workers) {
            const types = w.type === 'merge'
                ? (w.mergeTypes || [])
                : [w.type];
            for (const t of types) {
                if (!byAdapter.has(t)) {
                    byAdapter.set(t, {
                        id: t,
                        name: t,
                        models: [],
                        modelCount: 0,
                        textCount: 0,
                        imageCount: 0,
                        workers: [],
                        enabled: true
                    });
                }
                byAdapter.get(t).workers.push({
                    name: w.name,
                    instance: w.instanceName,
                    busy: (w.busyCount || 0) > 0,
                    busyCount: w.busyCount || 0
                });
            }
        }

        const providers = Array.from(byAdapter.values()).map(p => ({
            ...p,
            status: p.workers.some(w => w) ? 'active' : (p.modelCount > 0 ? 'registered' : 'idle'),
            runningWorkers: p.workers.length
        }));

        sendJson(res, 200, {
            object: 'providers.list',
            backend: backendName,
            count: providers.length,
            data: providers,
            timestamp: new Date().toISOString()
        });
    }

    /**
     * 处理 GET /v1/cookies
     */
    async function handleCookies(res, requestId, workerName, domain) {
        const poolContext = queueManager.getPoolContext();

        if (!poolContext?.poolManager) {
            sendApiError(res, { code: ERROR_CODES.BROWSER_NOT_INITIALIZED });
            return;
        }

        try {
            const result = await queueManager.getWorkerCookies(workerName, domain);
            sendJson(res, 200, {
                worker: result.worker,
                cookies: result.cookies
            });
        } catch (err) {
            logger.error('服务器', '获取 Cookies 失败', { id: requestId, error: err.message });

            if (err.message.includes('Worker 不存在') || err.message.includes('Worker not found')) {
                sendApiError(res, {
                    code: ERROR_CODES.INVALID_MODEL,
                    message: err.message
                });
            } else {
                sendApiError(res, {
                    code: ERROR_CODES.INTERNAL_ERROR,
                    message: err.message
                });
            }
        }
    }

    /**
     * 处理 chat completions（含 stateless 变体）
     * @param {boolean} stateless - 是否为无状态端点
     */
    async function handleChatCompletions(req, res, requestId, stateless = false) {
        const chunks = [];
        for await (const chunk of req) {
            chunks.push(chunk);
        }

        try {
            const body = Buffer.concat(chunks).toString();
            const data = JSON.parse(body);
            const isStreaming = data.stream === true;

            if (stateless) {
                // 借鉴 WebAI-to-API：stateless 由客户端自持完整历史
                // WebAI2API 文本模式本身就是虚拟上下文，这里显式标记并拒绝服务端续写依赖
                data.stateless = true;
                if (data.conversation_id) {
                    sendApiError(res, {
                        code: ERROR_CODES.INVALID_REQUEST_BODY,
                        message: 'stateless endpoint rejects conversation_id; client owns history',
                        status: 400
                    });
                    return;
                }
            }

            // 限流检查
            if (!isStreaming && !queueManager.canAcceptNonStreaming()) {
                const status = queueManager.getStatus();
                logger.warn('服务器', '非流式请求被拒绝 (队列已满)', { id: requestId, queueSize: status.total });
                sendApiError(res, {
                    code: ERROR_CODES.SERVER_BUSY,
                    message: `服务器繁忙（队列: ${status.total}/${queueManager.maxQueueSize}）。请使用流式模式 (stream: true) 或稍后重试。`
                });
                return;
            }

            // 设置 SSE 响应头
            if (isStreaming) {
                res.writeHead(200, {
                    'Content-Type': 'text/event-stream',
                    'Cache-Control': 'no-cache',
                    'Connection': 'keep-alive'
                });
            }

            // 解析请求
            const parseResult = await parseRequest(data, {
                tempDir,
                imageLimit,
                backendName,
                getSupportedModels: getModels,
                getImagePolicy,
                getModelType,
                requestId,
                logger,
                stateless
            });

            if (!parseResult.success) {
                sendApiError(res, {
                    code: parseResult.error.code,
                    message: parseResult.error.error,
                    isStreaming
                });
                return;
            }

            const { prompt, imagePaths, modelId, modelName } = parseResult.data;
            const reasoning = data.reasoning === true || stateless === true;

            logger.info('服务器', `[队列] 请求入队${stateless ? ' [stateless]' : ''}: ${prompt.slice(0, 100)}...`, {
                id: requestId,
                images: imagePaths.length
            });

            // 加入队列
            queueManager.addTask({
                req,
                res,
                prompt,
                imagePaths,
                modelId,
                modelName,
                id: requestId,
                isStreaming,
                reasoning,
                stateless
            });

        } catch (err) {
            logger.error('服务器', '请求处理失败', { id: requestId, error: err.message });
            sendApiError(res, {
                code: ERROR_CODES.INTERNAL_ERROR,
                message: err.message
            });
        }
    }

    /**
     * OpenAI API 路由处理函数
     * @param {import('http').IncomingMessage} req
     * @param {import('http').ServerResponse} res
     * @param {string} pathname - 去除 /v1 前缀后的路径
     * @param {URL} parsedUrl - 解析后的 URL 对象
     */
    return async function handleOpenAIRequest(req, res, pathname, parsedUrl) {
        const requestId = crypto.randomUUID().slice(0, 8);

        if (req.method === 'GET' && pathname === '/models') {
            handleModels(res);
        } else if (req.method === 'GET' && (pathname === '/openapi.json' || pathname === '/openapi')) {
            handleOpenApi(res);
        } else if (req.method === 'GET' && pathname === '/runtime/status') {
            handleRuntimeStatus(res);
        } else if (req.method === 'GET' && pathname === '/auth/status') {
            handleAuthStatus(res);
        } else if (req.method === 'GET' && pathname === '/providers') {
            handleProviders(res);
        } else if (req.method === 'GET' && pathname === '/cookies') {
            const workerName = parsedUrl.searchParams.get('name');
            const domain = parsedUrl.searchParams.get('domain');
            await handleCookies(res, requestId, workerName, domain);
        } else if (req.method === 'POST' && pathname === '/stateless/chat/completions') {
            await handleChatCompletions(req, res, requestId, true);
        } else if (req.method === 'POST' && pathname.startsWith('/chat/completions')) {
            await handleChatCompletions(req, res, requestId, false);
        } else if (req.method === 'GET' && pathname === '/stateless/models') {
            const models = getModels();
            const textModels = (models.data || []).filter(m => m.type === 'text');
            sendJson(res, 200, { object: 'list', data: textModels });
        } else {
            res.writeHead(404, { 'Content-Type': 'application/json' });
            res.end(JSON.stringify({
                error: {
                    message: `Unknown path: /v1${pathname}`,
                    type: 'invalid_request_error',
                    code: 'NOT_FOUND'
                }
            }));
        }
    };
}
