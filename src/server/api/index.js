/**
 * @fileoverview API 路由总装配
 * @description 统一挂载 /v1、/admin、/health、/ready、/openapi.json 路由
 * 升级点（借鉴 WebAI-to-API / WebModel）：
 * - 免鉴权健康检查端点
 * - Anthropic Messages 双协议
 * - OpenAPI schema
 */

import fs from 'fs';
import path from 'path';
import { createOpenAIRouter } from './openai/routes.js';
import { createAnthropicRouter } from './anthropic/routes.js';
import { createAdminRouter } from './admin/routes.js';
import { createAuthMiddleware } from '../middlewares/auth.js';
import { sendJson } from '../respond.js';
import { getSystemStatus } from '../../utils/systemInfo.js';
import { buildOpenApiSchema } from './openai/openapi.js';

// MIME 类型映射
const MIME_TYPES = {
    '.html': 'text/html; charset=utf-8',
    '.css': 'text/css; charset=utf-8',
    '.js': 'application/javascript; charset=utf-8',
    '.json': 'application/json; charset=utf-8',
    '.png': 'image/png',
    '.jpg': 'image/jpeg',
    '.jpeg': 'image/jpeg',
    '.gif': 'image/gif',
    '.svg': 'image/svg+xml',
    '.ico': 'image/x-icon',
    '.woff': 'font/woff',
    '.woff2': 'font/woff2',
    '.ttf': 'font/ttf'
};

// WebUI 静态文件目录
const WEBUI_DIR = path.join(process.cwd(), 'webui', 'dist');

// 服务版本缓存
let cachedVersion = '3.0.0';
try {
    const pkg = JSON.parse(fs.readFileSync(path.join(process.cwd(), 'package.json'), 'utf8'));
    cachedVersion = pkg.version || cachedVersion;
} catch { /* ignore */ }

/**
 * 创建全局路由处理器
 * @param {object} context - 路由上下文
 * @param {boolean} [context.loginMode] - 登录模式（禁用 OpenAI API）
 * @returns {Function} 请求处理函数
 */
export function createGlobalRouter(context) {
    const { authToken, config, queueManager, tempDir, loginMode, getSafeMode } = context;

    // 创建鉴权中间件
    const checkAuth = createAuthMiddleware(authToken);

    // 创建子路由处理器
    const handleOpenAIRequest = loginMode ? null : createOpenAIRouter(context);
    const handleAnthropicRequest = loginMode ? null : createAnthropicRouter(context);
    const handleAdminRequest = createAdminRouter({ config, queueManager, tempDir, getSafeMode });

    /**
     * 免鉴权健康检查（liveness）
     * 借鉴 WebAI-to-API: GET /health
     */
    function handleHealth(res) {
        const system = getSystemStatus();
        sendJson(res, 200, {
            status: 'ok',
            service: 'webai2api',
            version: system.version,
            uptime: system.uptime,
            timestamp: new Date().toISOString()
        });
    }

    /**
     * 免鉴权就绪检查
     * 借鉴 WebAI-to-API: GET /ready
     */
    function handleReady(res) {
        const safeMode = getSafeMode?.() || { enabled: false, reason: null };
        const poolContext = queueManager?.getPoolContext?.();
        const poolReady = !!poolContext?.poolManager;
        const ready = !safeMode.enabled && !loginMode && poolReady;

        sendJson(res, ready ? 200 : 503, {
            ready,
            service: 'webai2api',
            safeMode: safeMode.enabled,
            safeModeReason: safeMode.reason || null,
            loginMode: !!loginMode,
            poolReady,
            timestamp: new Date().toISOString()
        });
    }

    /**
     * 免鉴权 OpenAPI schema
     * 借鉴 WebAI-to-API
     */
    function handleOpenApiRoot(res) {
        let models = { object: 'list', data: [] };
        try {
            models = context.getModels?.() || models;
        } catch { /* pool not ready */ }
        sendJson(res, 200, buildOpenApiSchema({ models, version: cachedVersion }));
    }

    /**
     * 主路由处理函数
     */
    return async function handleRequest(req, res) {
        const parsedUrl = new URL(req.url, `http://${req.headers.host}`);
        const pathname = parsedUrl.pathname;

        // ==================== 免鉴权系统端点 ====================
        if (req.method === 'GET') {
            if (pathname === '/health' || pathname === '/healthz') {
                handleHealth(res);
                return;
            }
            if (pathname === '/ready' || pathname === '/readyz') {
                handleReady(res);
                return;
            }
            if (pathname === '/openapi.json' || pathname === '/openapi') {
                handleOpenApiRoot(res);
                return;
            }
        }

        // ==================== 静态文件服务 ====================
        if (req.method === 'GET' && !pathname.startsWith('/v1') && !pathname.startsWith('/admin')) {
            let filePath = pathname === '/' ? '/index.html' : pathname;
            filePath = path.join(WEBUI_DIR, filePath);

            // 安全检查
            if (!filePath.startsWith(WEBUI_DIR)) {
                res.writeHead(403);
                res.end('Forbidden');
                return;
            }

            // 检查文件是否存在
            if (fs.existsSync(filePath) && fs.statSync(filePath).isFile()) {
                const ext = path.extname(filePath).toLowerCase();
                const contentType = MIME_TYPES[ext] || 'application/octet-stream';
                const content = fs.readFileSync(filePath);
                res.writeHead(200, { 'Content-Type': contentType });
                res.end(content);
                return;
            }

            // SPA 模式 fallback
            const indexPath = path.join(WEBUI_DIR, 'index.html');
            if (fs.existsSync(indexPath)) {
                const content = fs.readFileSync(indexPath);
                res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
                res.end(content);
                return;
            }
        }

        // ==================== 鉴权检查 ====================
        if (!checkAuth(req, res)) {
            return; // 鉴权失败，已发送错误响应
        }

        // ==================== API 路由分发 ====================

        // Admin API (/admin)
        if (pathname.startsWith('/admin')) {
            const adminPath = pathname.slice(6); // 去除 /admin 前缀
            await handleAdminRequest(req, res, adminPath);
            return;
        }

        // OpenAI / Anthropic API (/v1)
        if (pathname.startsWith('/v1')) {
            // 安全模式下禁用生成 API（但保留 runtime/status 等诊断端点）
            const safeMode = getSafeMode?.();
            const isDiagnostic = /\/(models|runtime\/status|auth\/status|providers|openapi)/.test(pathname)
                && req.method === 'GET';

            if (safeMode?.enabled && !isDiagnostic) {
                const isAnthropic = pathname.startsWith('/v1/messages');
                if (isAnthropic) {
                    res.writeHead(503, { 'Content-Type': 'application/json' });
                    res.end(JSON.stringify({
                        type: 'error',
                        error: {
                            type: 'overloaded_error',
                            message: `Service in safe mode: ${safeMode.reason}`
                        }
                    }));
                } else {
                    res.writeHead(503, { 'Content-Type': 'application/json' });
                    res.end(JSON.stringify({
                        error: {
                            message: `服务运行在安全模式，OpenAI API 不可用。原因: ${safeMode.reason}`,
                            type: 'service_unavailable',
                            code: 'SERVICE_UNAVAILABLE'
                        }
                    }));
                }
                return;
            }
            // 登录模式下禁用生成 API
            if (!handleOpenAIRequest) {
                const isAnthropic = pathname.startsWith('/v1/messages');
                if (isAnthropic) {
                    res.writeHead(503, { 'Content-Type': 'application/json' });
                    res.end(JSON.stringify({
                        type: 'error',
                        error: { type: 'overloaded_error', message: 'Service in login mode, generation API unavailable' }
                    }));
                } else {
                    res.writeHead(503, { 'Content-Type': 'application/json' });
                    res.end(JSON.stringify({
                        error: { message: '服务运行在登录模式，OpenAI API 不可用', type: 'service_unavailable' }
                    }));
                }
                return;
            }
            const v1Path = pathname.slice(3); // 去除 /v1 前缀

            // Anthropic Messages 双协议
            if (v1Path.startsWith('/messages') && handleAnthropicRequest) {
                const handled = await handleAnthropicRequest(req, res, v1Path);
                if (handled) return;
            }

            await handleOpenAIRequest(req, res, v1Path, parsedUrl);
            return;
        }

        // 404
        res.writeHead(404, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ error: { message: 'Not Found', code: 'NOT_FOUND' } }));
    };
}
