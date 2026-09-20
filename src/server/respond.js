/**
 * @fileoverview 统一响应写出模块
 * @description 封装 JSON、SSE 响应和错误响应的统一处理函数
 */

import { getErrorDetails } from './errors.js';

/**
 * 发送 JSON 响应
 * @param {import('http').ServerResponse} res - HTTP 响应对象
 * @param {number} status - HTTP 状态码
 * @param {object} payload - 响应数据
 */
export function sendJson(res, status, payload) {
    if (res.writableEnded) return;
    res.writeHead(status, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify(payload));
}

/**
 * 发送 SSE 事件
 * @param {import('http').ServerResponse} res - HTTP 响应对象
 * @param {object} payload - 事件数据
 */
export function sendSse(res, payload) {
    if (res.writableEnded) return;
    res.write(`data: ${JSON.stringify(payload)}\n\n`);
}

/**
 * 发送 SSE 结束标记
 * @param {import('http').ServerResponse} res - HTTP 响应对象
 */
export function sendSseDone(res) {
    if (res.writableEnded) return;
    res.write(`data: [DONE]\n\n`);
    res.end();
}

/**
 * 发送 SSE 心跳包
 * @param {import('http').ServerResponse} res - HTTP 响应对象
 * @param {string} mode - 心跳模式 ('comment' | 'content')
 * @param {string} [modelName] - 模型名称（content 模式需要）
 */
export function sendHeartbeat(res, mode, modelName) {
    if (res.writableEnded) return;

    if (mode === 'comment') {
        res.write(`:keepalive\n\n`);
    } else {
        // content 模式：发送空 delta
        const chunk = {
            id: 'chatcmpl-' + Date.now(),
            object: 'chat.completion.chunk',
            created: Math.floor(Date.now() / 1000),
            model: modelName || 'default-model',
            choices: [{
                index: 0,
                delta: { content: '' },
                finish_reason: null
            }]
        };
        res.write(`data: ${JSON.stringify(chunk)}\n\n`);
    }
}

/**
 * 发送统一 API 错误响应 (OpenAI 标准格式)
 * @param {import('http').ServerResponse} res - HTTP 响应对象
 * @param {object} options - 错误选项
 * @param {string} [options.code] - 错误码（使用 ERROR_CODES 枚举）
 * @param {string} [options.message] - 自定义错误消息（如提供则覆盖 code 对应的消息）
 * @param {number} [options.status] - 自定义 HTTP 状态码
 * @param {boolean} [options.isStreaming=false] - 是否为流式响应
 */
export function sendApiError(res, options) {
    const { code, message, status, isStreaming = false } = options;

    // 获取错误详情
    const details = code ? getErrorDetails(code) : null;
    const errorMessage = message || (details ? details.message : '未知错误');
    const errorType = details?.type || 'server_error';
    const httpStatus = status || (details ? details.status : 500);

    // 构造 OpenAI 标准错误响应体
    const payload = {
        error: {
            message: errorMessage,
            type: errorType,
            code: code || 'INTERNAL_ERROR'
        }
    };

    if (isStreaming) {
        // 流式响应：发送错误事件然后结束
        sendSse(res, payload);
        sendSseDone(res);
    } else {
        // 非流式响应
        sendJson(res, httpStatus, payload);
    }
}

/**
 * 构造 OpenAI 格式的聊天完成响应（非流式）
 * @param {string} content - 响应内容
 * @param {string} [modelName] - 模型名称
 * @param {string} [reasoningContent] - 思考/推理过程内容 (OpenAI o1 格式)
 * @returns {object} OpenAI 格式的响应对象
 */
export function buildChatCompletion(content, modelName, reasoningContent) {
    const message = {
        role: 'assistant',
        content: content
    };
    if (reasoningContent) {
        message.reasoning_content = reasoningContent;
    }

    return {
        id: 'chatcmpl-' + Date.now(),
        object: 'chat.completion',
        created: Math.floor(Date.now() / 1000),
        model: modelName || 'default-model',
        choices: [{
            index: 0,
            message,
            finish_reason: 'stop'
        }]
    };
}

/**
 * 构造 OpenAI 格式的流式聊天完成响应块
 * @param {string} content - 响应内容
 * @param {string} [modelName] - 模型名称
 * @param {string|null} [finishReason='stop'] - 完成原因
 * @param {string} [reasoningContent] - 思考/推理过程内容 (OpenAI o1 格式)
 * @returns {object} OpenAI 格式的流式响应块
 */
export function buildChatCompletionChunk(content, modelName, finishReason = 'stop', reasoningContent) {
    const delta = { content };
    if (reasoningContent) {
        delta.reasoning_content = reasoningContent;
    }

    return {
        id: 'chatcmpl-' + Date.now(),
        object: 'chat.completion.chunk',
        created: Math.floor(Date.now() / 1000),
        model: modelName || 'default-model',
        choices: [{
            index: 0,
            delta,
            finish_reason: finishReason
        }]
    };
}

/**
 * 构造 Anthropic Messages API 响应（非流式）
 * 借鉴 WebModel 双协议设计，便于 Claude Code 等客户端接入
 * @param {string} content - 响应文本
 * @param {string} [modelName] - 模型名称
 * @param {string} [reasoningContent] - 思考过程内容
 * @param {number} [inputTokens=0] - 估算输入 token
 * @returns {object} Anthropic messages 响应
 */
export function buildAnthropicMessage(content, modelName, reasoningContent, inputTokens = 0) {
    const blocks = [];
    if (reasoningContent) {
        blocks.push({ type: 'thinking', thinking: reasoningContent });
    }
    blocks.push({ type: 'text', text: content || '' });

    return {
        id: `msg_${Date.now().toString(36)}`,
        type: 'message',
        role: 'assistant',
        model: modelName || 'default-model',
        content: blocks,
        stop_reason: 'end_turn',
        stop_sequence: null,
        usage: {
            input_tokens: inputTokens || Math.max(1, Math.floor((content || '').length / 4)),
            output_tokens: Math.max(1, Math.floor((content || '').length / 4))
        }
    };
}

/**
 * 构造 Anthropic Messages API 流式事件
 * @param {string} content - 响应文本
 * @param {string} [modelName] - 模型名称
 * @param {string} [reasoningContent] - 思考过程内容
 * @returns {string[]} SSE 事件块列表
 */
export function buildAnthropicMessageEvents(content, modelName, reasoningContent) {
    const events = [];
    const msgId = `msg_${Date.now().toString(36)}`;
    const model = modelName || 'default-model';

    events.push(`event: message_start\ndata: ${JSON.stringify({
        type: 'message_start',
        message: {
            id: msgId,
            type: 'message',
            role: 'assistant',
            model,
            content: [],
            stop_reason: null,
            stop_sequence: null,
            usage: { input_tokens: 1, output_tokens: 0 }
        }
    })}\n\n`);

    if (reasoningContent) {
        events.push(`event: content_block_start\ndata: ${JSON.stringify({
            type: 'content_block_start',
            index: 0,
            content_block: { type: 'thinking', thinking: '' }
        })}\n\n`);
        events.push(`event: content_block_delta\ndata: ${JSON.stringify({
            type: 'content_block_delta',
            index: 0,
            delta: { type: 'thinking_delta', thinking: reasoningContent }
        })}\n\n`);
        events.push(`event: content_block_stop\ndata: ${JSON.stringify({
            type: 'content_block_stop',
            index: 0
        })}\n\n`);
        events.push(`event: content_block_start\ndata: ${JSON.stringify({
            type: 'content_block_start',
            index: 1,
            content_block: { type: 'text', text: '' }
        })}\n\n`);
        events.push(`event: content_block_delta\ndata: ${JSON.stringify({
            type: 'content_block_delta',
            index: 1,
            delta: { type: 'text_delta', text: content || '' }
        })}\n\n`);
        events.push(`event: content_block_stop\ndata: ${JSON.stringify({
            type: 'content_block_stop',
            index: 1
        })}\n\n`);
    } else {
        events.push(`event: content_block_start\ndata: ${JSON.stringify({
            type: 'content_block_start',
            index: 0,
            content_block: { type: 'text', text: '' }
        })}\n\n`);
        events.push(`event: content_block_delta\ndata: ${JSON.stringify({
            type: 'content_block_delta',
            index: 0,
            delta: { type: 'text_delta', text: content || '' }
        })}\n\n`);
        events.push(`event: content_block_stop\ndata: ${JSON.stringify({
            type: 'content_block_stop',
            index: 0
        })}\n\n`);
    }

    events.push(`event: message_delta\ndata: ${JSON.stringify({
        type: 'message_delta',
        delta: { stop_reason: 'end_turn', stop_sequence: null },
        usage: { output_tokens: Math.max(1, Math.floor((content || '').length / 4)) }
    })}\n\n`);

    events.push(`event: message_stop\ndata: ${JSON.stringify({
        type: 'message_stop'
    })}\n\n`);

    return events;
}

/**
 * 发送 Anthropic 格式错误响应
 * @param {import('http').ServerResponse} res - HTTP 响应对象
 * @param {object} options - 错误选项
 * @param {string} [options.code] - 错误码
 * @param {string} [options.message] - 错误消息
 * @param {number} [options.status] - HTTP 状态码
 * @param {boolean} [options.isStreaming=false] - 是否流式
 */
export function sendAnthropicError(res, options) {
    const { code, message, status, isStreaming = false } = options;
    const details = code ? getErrorDetails(code) : null;
    const errorMessage = message || details?.message || 'unknown error';
    const httpStatus = status || details?.status || 500;

    // Anthropic 错误类型映射
    const typeMap = {
        400: 'invalid_request_error',
        401: 'authentication_error',
        403: 'permission_error',
        404: 'not_found_error',
        429: 'rate_limit_error',
        500: 'api_error',
        502: 'api_error',
        503: 'overloaded_error'
    };

    const payload = {
        type: 'error',
        error: {
            type: typeMap[httpStatus] || 'api_error',
            message: errorMessage
        }
    };

    if (isStreaming) {
        if (!res.writableEnded) {
            res.write(`event: error\ndata: ${JSON.stringify(payload)}\n\n`);
            res.end();
        }
    } else {
        sendJson(res, httpStatus, payload);
    }
}
