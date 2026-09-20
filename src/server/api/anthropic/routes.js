/**
 * @fileoverview Anthropic Messages API 路由
 * @description 借鉴 WebModel 双协议设计：POST /v1/messages
 * 兼容 Claude Code / Anthropic SDK 客户端，复用内部队列与生成管线
 */

import crypto from 'crypto';
import { logger } from '../../../utils/logger.js';
import { ERROR_CODES } from '../../errors.js';
import { parseRequest } from '../openai/parse.js';
import {
    buildAnthropicMessage,
    buildAnthropicMessageEvents,
    sendAnthropicError
} from '../../respond.js';

/**
 * 将 Anthropic 请求体转换为内部 OpenAI-like 解析格式
 * @param {object} body - Anthropic Messages 请求体
 * @returns {object} 内部解析格式
 */
export function convertAnthropicToInternal(body) {
    const messages = [];

    // system 可以是 string 或 content blocks
    if (body.system) {
        let systemText = '';
        if (typeof body.system === 'string') {
            systemText = body.system;
        } else if (Array.isArray(body.system)) {
            systemText = body.system
                .filter(b => b.type === 'text')
                .map(b => b.text)
                .join('\n');
        }
        if (systemText) {
            messages.push({ role: 'system', content: systemText });
        }
    }

    // messages 数组
    for (const msg of body.messages || []) {
        const role = msg.role;
        let content;

        if (typeof msg.content === 'string') {
            content = msg.content;
        } else if (Array.isArray(msg.content)) {
            const parts = [];
            for (const block of msg.content) {
                if (block.type === 'text') {
                    parts.push({ type: 'text', text: block.text || '' });
                } else if (block.type === 'image' && block.source?.data) {
                    const mediaType = block.source.media_type || 'image/png';
                    parts.push({
                        type: 'image_url',
                        image_url: { url: `data:${mediaType};base64,${block.source.data}` }
                    });
                } else if (block.type === 'tool_result') {
                    const toolText = typeof block.content === 'string'
                        ? block.content
                        : JSON.stringify(block.content || '');
                    parts.push({ type: 'text', text: `[tool_result] ${toolText}` });
                }
            }
            content = parts.length === 1 && parts[0].type === 'text'
                ? parts[0].text
                : parts;
        } else {
            content = String(msg.content ?? '');
        }

        // tool_use 块序列化为文本上下文（WebAI 走浏览器模拟，不原生执行 tool）
        if (Array.isArray(msg.content)) {
            const toolUses = msg.content.filter(b => b.type === 'tool_use');
            if (toolUses.length > 0 && typeof content === 'string') {
                content = content + '\n' + toolUses
                    .map(t => `[tool_use:${t.name}] ${JSON.stringify(t.input || {})}`)
                    .join('\n');
            }
        }

        messages.push({ role, content });
    }

    return {
        model: body.model,
        messages,
        stream: body.stream === true,
        // Anthropic 请求强制走客户端自持历史（stateless）
        stateless: true,
        reasoning: true
    };
}

/**
 * 创建 Anthropic API 路由处理器
 * @param {object} context - 路由上下文
 * @returns {Function} 路由处理函数
 */
export function createAnthropicRouter(context) {
    const {
        backendName,
        getModels,
        getImagePolicy,
        getModelType,
        tempDir,
        imageLimit,
        queueManager,
        getSafeMode
    } = context;

    /**
     * 处理 POST /v1/messages
     */
    async function handleMessages(req, res, requestId) {
        const chunks = [];
        for await (const chunk of req) {
            chunks.push(chunk);
        }

        let data;
        try {
            const body = Buffer.concat(chunks).toString();
            data = JSON.parse(body);
        } catch (err) {
            sendAnthropicError(res, {
                code: ERROR_CODES.INVALID_REQUEST_BODY,
                message: `Invalid JSON body: ${err.message}`
            });
            return;
        }

        const isStreaming = data.stream === true;

        // 安全模式检查
        const safeMode = getSafeMode?.();
        if (safeMode?.enabled) {
            sendAnthropicError(res, {
                code: ERROR_CODES.SERVICE_UNAVAILABLE,
                message: `Service in safe mode: ${safeMode.reason}`
            });
            return;
        }

        // 限流检查
        if (!isStreaming && !queueManager.canAcceptNonStreaming()) {
            const status = queueManager.getStatus();
            sendAnthropicError(res, {
                code: ERROR_CODES.SERVER_BUSY,
                message: `Server busy (queue: ${status.total}). Use stream: true or retry later.`
            });
            return;
        }

        const internal = convertAnthropicToInternal(data);
        const modelHint = internal.model;

        // SSE 头
        if (isStreaming && !res.writableEnded) {
            res.writeHead(200, {
                'Content-Type': 'text/event-stream',
                'Cache-Control': 'no-cache',
                'Connection': 'keep-alive',
                'x-accel-buffering': 'no'
            });
        }

        let parseResult;
        try {
            parseResult = await parseRequest(internal, {
                tempDir,
                imageLimit,
                backendName,
                getSupportedModels: getModels,
                getImagePolicy,
                getModelType,
                requestId,
                logger
            });
        } catch (err) {
            sendAnthropicError(res, {
                code: ERROR_CODES.INTERNAL_ERROR,
                message: err.message,
                isStreaming
            });
            return;
        }

        if (!parseResult.success) {
            sendAnthropicError(res, {
                code: parseResult.error.code,
                message: parseResult.error.error,
                isStreaming
            });
            return;
        }

        const { prompt, imagePaths, modelId, modelName } = parseResult.data;

        // 协议转换包装：拦截 OpenAI 响应，改写为 Anthropic 格式
        const originalRes = res;
        const protocolAdapter = {
            writeHead(status, headers) {
                if (originalRes.writableEnded) return;
                if (isStreaming) {
                    // 已在前面写过 SSE 头，忽略重复
                    return;
                }
                this._pendingStatus = status;
                this._pendingHeaders = headers;
            },
            write(chunk) {
                if (originalRes.writableEnded) return;
                if (isStreaming) {
                    // 心跳注释透传
                    if (typeof chunk === 'string' && chunk.startsWith(':')) {
                        originalRes.write(chunk);
                        return;
                    }
                    if (typeof chunk === 'string' && chunk.startsWith('data: ') && !chunk.includes('[DONE]')) {
                        try {
                            const payload = JSON.parse(chunk.slice(6));
                            if (payload?.error) {
                                this._streamError = payload.error;
                                return;
                            }
                            const delta = payload?.choices?.[0]?.delta;
                            if (delta?.content) {
                                this._bufferedContent = (this._bufferedContent || '') + delta.content;
                            }
                            if (delta?.reasoning_content) {
                                this._bufferedReasoning = (this._bufferedReasoning || '')
                                    + delta.reasoning_content;
                            }
                        } catch { /* ignore partial */ }
                    }
                    return;
                }
                this._bufferedWrite = (this._bufferedWrite || '') + chunk.toString();
            },
            end(payload) {
                if (originalRes.writableEnded) return;

                if (isStreaming) {
                    if (this._streamError) {
                        originalRes.write(`event: error\ndata: ${JSON.stringify({
                            type: 'error',
                            error: {
                                type: 'api_error',
                                message: this._streamError.message || 'generation failed'
                            }
                        })}\n\n`);
                        originalRes.end();
                        return;
                    }
                    const text = this._bufferedContent || '';
                    const reasoning = this._bufferedReasoning || null;
                    const events = buildAnthropicMessageEvents(text, modelName || modelHint, reasoning);
                    for (const evt of events) {
                        originalRes.write(evt);
                    }
                    originalRes.end();
                    logger.info('服务器', 'Anthropic 流式响应已结束', { id: requestId });
                    return;
                }

                // 非流式：解析 sendJson 写入的 OpenAI 格式
                const raw = payload ? payload.toString() : this._bufferedWrite || '';
                let text = '';
                let reasoning = null;
                let isError = false;
                let errorPayload = null;

                if (raw) {
                    try {
                        const dataObj = JSON.parse(raw);
                        if (dataObj.error) {
                            isError = true;
                            errorPayload = dataObj;
                        } else {
                            text = dataObj?.choices?.[0]?.message?.content || '';
                            reasoning = dataObj?.choices?.[0]?.message?.reasoning_content || null;
                        }
                    } catch { /* empty */ }
                }

                if (isError && errorPayload) {
                    originalRes.writeHead(502, { 'Content-Type': 'application/json' });
                    originalRes.end(JSON.stringify({
                        type: 'error',
                        error: {
                            type: 'api_error',
                            message: errorPayload.error?.message || 'generation failed'
                        }
                    }));
                    return;
                }

                const anthropicResp = buildAnthropicMessage(text, modelName || modelHint, reasoning);
                originalRes.writeHead(200, { 'Content-Type': 'application/json' });
                originalRes.end(JSON.stringify(anthropicResp));
                logger.info('服务器', 'Anthropic JSON 响应已发送', { id: requestId });
            },
            get writableEnded() { return originalRes.writableEnded; }
        };

        // 入队：用协议适配器替换 res
        // 心跳由 queueManager 统一发送，protocolAdapter.write 会透传 comment 心跳
        queueManager.addTask({
            req,
            res: protocolAdapter,
            prompt,
            imagePaths,
            modelId,
            modelName: modelName || modelHint,
            id: requestId,
            isStreaming,
            reasoning: true,
            stateless: true
        });
    }

    return async function handleAnthropicRequest(req, res, pathname) {
        const requestId = crypto.randomUUID().slice(0, 8);

        if (req.method === 'POST' && (pathname === '/messages' || pathname.startsWith('/messages'))) {
            await handleMessages(req, res, requestId);
            return true;
        }

        // GET /v1/messages 不支持
        if (req.method === 'GET' && pathname.startsWith('/messages')) {
            sendAnthropicError(res, {
                code: ERROR_CODES.INVALID_REQUEST_BODY,
                message: 'Method GET not allowed. Use POST /v1/messages'
            });
            return true;
        }

        return false;
    };
}
