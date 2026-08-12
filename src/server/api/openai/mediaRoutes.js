/**
 * OpenAI-style asynchronous media and file routes.
 */

import crypto from 'crypto';
import fs from 'fs';
import path from 'path';
import { sendJson, sendApiError } from '../../respond.js';
import { ERROR_CODES } from '../../errors.js';

const MAX_JSON_BYTES = 2 * 1024 * 1024;

async function readBody(req, maxBytes = MAX_JSON_BYTES) {
    const length = Number(req.headers['content-length'] || 0);
    if (length > maxBytes) throw new Error(`请求体超过 ${Math.floor(maxBytes / 1024 / 1024)}MB 限制`);
    const chunks = [];
    let total = 0;
    for await (const chunk of req) {
        total += chunk.length;
        if (total > maxBytes) throw new Error(`请求体超过 ${Math.floor(maxBytes / 1024 / 1024)}MB 限制`);
        chunks.push(chunk);
    }
    return Buffer.concat(chunks);
}

async function readJson(req) {
    const body = await readBody(req);
    try {
        return JSON.parse(body.toString('utf8'));
    } catch {
        throw new Error('请求体不是有效 JSON');
    }
}

function getModel(models, model, expectedType) {
    if (!model || typeof model !== 'string') throw new Error('model 是必填字段');
    const found = models().data.find(item => item.id === model);
    if (!found) throw new Error(`模型无效/当前服务不支持: ${model}`);
    if (expectedType && found.type !== expectedType) {
        throw new Error(`模型 ${model} 的类型为 ${found.type}，不能用于 ${expectedType} 接口`);
    }
    return found;
}

function extractPrompt(data) {
    const prompt = data.prompt ?? data.input;
    if (typeof prompt !== 'string' || !prompt.trim()) throw new Error('prompt 或 input 是必填字段');
    return prompt.trim();
}

function normalizeInputFiles(data) {
    const raw = data.input_reference ?? data.input_reference_ids ?? data.reference_images ?? data.file_ids ?? [];
    const values = Array.isArray(raw) ? raw : [raw];
    const ids = values.map(item => typeof item === 'string' ? item : item?.file_id || item?.id).filter(Boolean);
    if (ids.some(id => !String(id).startsWith('file_'))) throw new Error('引用图必须先通过 /v1/files 上传并使用 file_id');
    return [...new Set(ids.map(String))];
}

function parseRange(rangeHeader, size) {
    if (!rangeHeader) return null;
    const match = /^bytes=(\d*)-(\d*)$/.exec(rangeHeader);
    if (!match) return 'invalid';
    let start = match[1] ? Number(match[1]) : 0;
    let end = match[2] ? Number(match[2]) : size - 1;
    if (!Number.isInteger(start) || !Number.isInteger(end) || start < 0 || end < start || start >= size) return 'invalid';
    end = Math.min(end, size - 1);
    return { start, end };
}

async function serveContent(req, res, mediaManager, jobId, kind) {
    const job = mediaManager.getJob(jobId);
    if (!job || job.kind !== kind) {
        sendApiError(res, { code: ERROR_CODES.INVALID_MODEL, message: '媒体任务不存在' });
        return;
    }
    if (job.status !== 'completed') {
        sendJson(res, 409, mediaManager.formatJob(job));
        return;
    }
    const content = mediaManager.getContent(job);
    if (!content) {
        sendApiError(res, { code: ERROR_CODES.GENERATION_FAILED, message: '媒体文件已过期或不可用' });
        return;
    }
    const stat = await fs.promises.stat(content.absolutePath);
    const range = parseRange(req.headers.range, stat.size);
    if (range === 'invalid') {
        res.writeHead(416, { 'Content-Range': `bytes */${stat.size}` });
        res.end();
        return;
    }
    const headers = {
        'Content-Type': content.mime_type || 'application/octet-stream',
        'Accept-Ranges': 'bytes',
        'Cache-Control': 'private, max-age=0',
        'Content-Disposition': `inline; filename="${path.basename(content.absolutePath)}"`
    };
    if (range) {
        headers['Content-Range'] = `bytes ${range.start}-${range.end}/${stat.size}`;
        headers['Content-Length'] = range.end - range.start + 1;
        res.writeHead(206, headers);
        if (req.method !== 'HEAD') fs.createReadStream(content.absolutePath, range).pipe(res);
        else res.end();
        return;
    }
    headers['Content-Length'] = stat.size;
    res.writeHead(200, headers);
    if (req.method !== 'HEAD') fs.createReadStream(content.absolutePath).pipe(res);
    else res.end();
}

function parseMultipart(buffer, contentType) {
    const boundaryMatch = /boundary=(?:"([^"]+)"|([^;\s]+))/i.exec(contentType || '');
    if (!boundaryMatch) throw new Error('multipart 请求缺少 boundary');
    const delimiter = Buffer.from(`--${boundaryMatch[1] || boundaryMatch[2]}`);
    const headerSeparator = Buffer.from('\r\n\r\n');
    const parts = [];
    let cursor = buffer.indexOf(delimiter);
    if (cursor < 0) throw new Error('multipart boundary 无效');
    cursor += delimiter.length + 2;
    while (cursor < buffer.length) {
        const next = buffer.indexOf(delimiter, cursor);
        if (next < 0) break;
        const part = buffer.subarray(cursor, next - 2);
        const headerEnd = part.indexOf(headerSeparator);
        if (headerEnd >= 0) {
            const headers = part.subarray(0, headerEnd).toString('utf8');
            const body = part.subarray(headerEnd + headerSeparator.length);
            const disposition = /content-disposition:\s*form-data;([^\r\n]+)/i.exec(headers)?.[1] || '';
            const name = /name="([^"]+)"/i.exec(disposition)?.[1];
            const filename = /filename="([^"]*)"/i.exec(disposition)?.[1];
            const mimeType = /content-type:\s*([^\r\n]+)/i.exec(headers)?.[1]?.trim();
            parts.push({ name, filename, mimeType, body });
        }
        cursor = next + delimiter.length;
        if (buffer.subarray(cursor, cursor + 2).toString() === '--') break;
        cursor += 2;
    }
    return parts;
}

function transcriptionFile(parts) {
    const file = parts.find(part => part.name === 'file' && part.filename !== undefined);
    if (!file?.filename) throw new Error('multipart 请求必须包含音频 file 字段');
    return file;
}

function clientError(message) {
    const error = new Error(message);
    error.status = 400;
    return error;
}

function redactTranscriptionError(message, uploadedPath) {
    let redacted = String(message || '未知错误');
    if (uploadedPath) redacted = redacted.replaceAll(String(uploadedPath), '已上传音频');
    return redacted
        .replace(/(?:[A-Za-z]:\\|\/)(?:[^\s"'<>\\/]+[\\/])*[^\s"'<>]+/g, '[private path]')
        .replace(/\b(api[_-]?key|token|authorization)\s*[=:]\s*[^\s,;]+/gi, (_match, name) => `${name}=[redacted]`);
}

/**
 * @param {object} context
 */
export function createMediaRouter(context) {
    const { getModels, mediaManager, transcribe } = context;

    async function create(kind, req, res) {
        try {
            const data = await readJson(req);
            const model = getModel(getModels, data.model, kind);
            const prompt = extractPrompt(data);
            const options = {
                seconds: Number(data.seconds ?? data.duration ?? 0) || undefined,
                size: data.size || data.ratio || undefined,
                n: Number(data.n || 1),
                provider_options: data.provider_options || {}
            };
            if (options.seconds && (!Number.isInteger(options.seconds) || options.seconds < 1 || options.seconds > 30)) {
                throw new Error('seconds 必须是 1-30 的整数');
            }
            const { job, reused } = mediaManager.createJob(kind, {
                model: model.id,
                prompt,
                options,
                inputFileIds: normalizeInputFiles(data)
            }, { idempotencyKey: req.headers['idempotency-key'] });
            sendJson(res, reused ? 200 : 202, mediaManager.formatJob(job));
        } catch (error) {
            sendApiError(res, { code: ERROR_CODES.INVALID_MODEL, message: error.message, status: 400 });
        }
    }

    async function uploadFile(req, res) {
        try {
            const body = await readBody(req, mediaManager.maxUploadBytes + 1024 * 1024);
            const parts = parseMultipart(body, req.headers['content-type']);
            const filePart = parts.find(part => part.name === 'file' && part.filename !== undefined);
            if (!filePart || !filePart.filename) throw new Error('multipart 请求必须包含 file 字段');
            const purpose = parts.find(part => part.name === 'purpose')?.body?.toString('utf8') || 'user_data';
            const file = await mediaManager.saveUpload({
                filename: filePart.filename,
                mimeType: filePart.mimeType || 'application/octet-stream',
                buffer: filePart.body,
                purpose
            });
            sendJson(res, 201, {
                id: file.id,
                object: 'file',
                bytes: file.size_bytes,
                created_at: file.created_at,
                filename: file.filename,
                purpose: file.purpose,
                mime_type: file.mime_type
            });
        } catch (error) {
            sendApiError(res, { code: ERROR_CODES.INVALID_MODEL, message: error.message, status: 400 });
        }
    }

    async function createTranscription(req, res) {
        try {
            if (typeof transcribe !== 'function') throw new Error('当前服务未配置录音转写能力');
            let audio;
            let modelInfo;
            let responseFormat;
            try {
                const body = await readBody(req, mediaManager.maxUploadBytes + 1024 * 1024);
                const parts = parseMultipart(body, req.headers['content-type']);
                audio = transcriptionFile(parts);
                const model = parts.find(part => part.name === 'model')?.body?.toString('utf8')?.trim() || 'doubao-transcription';
                modelInfo = getModel(getModels, model, 'transcription');
                responseFormat = parts.find(part => part.name === 'response_format')?.body?.toString('utf8')?.trim() || 'json';
                if (!['json', 'text', 'verbose_json'].includes(responseFormat)) {
                    throw clientError('response_format 仅支持 json、text 或 verbose_json');
                }
            } catch (error) {
                error.status ||= 400;
                throw error;
            }
            const file = await mediaManager.saveUpload({
                filename: audio.filename,
                mimeType: audio.mimeType || 'application/octet-stream',
                buffer: audio.body,
                purpose: 'transcription'
            });
            const result = await transcribe({
                filePath: file.absolute_path,
                file,
                responseFormat
            }, modelInfo.id, { requestId: crypto.randomUUID().slice(0, 8) });
            if (result?.error) throw new Error(redactTranscriptionError(result.error, file.absolute_path));
            const text = String(result?.text || '').trim();
            if (!text) throw new Error('豆包未返回转写文本');
            if (responseFormat === 'text') {
                res.writeHead(200, { 'Content-Type': 'text/plain; charset=utf-8' });
                res.end(text);
                return;
            }
            sendJson(res, 200, responseFormat === 'verbose_json'
                ? { task: 'transcribe', language: result.language || null, duration: result.duration || null, text }
                : { text });
        } catch (error) {
            const status = error.status || 502;
            sendApiError(res, {
                code: status === 400 ? ERROR_CODES.INVALID_MODEL : ERROR_CODES.GENERATION_FAILED,
                message: error.message,
                status
            });
        }
    }

    return async function handleMediaRequest(req, res, pathname) {
        if (req.method === 'POST' && pathname === '/files') return uploadFile(req, res);
        if (req.method === 'POST' && pathname === '/audio/transcriptions') return createTranscription(req, res);
        if (req.method === 'POST' && (pathname === '/videos' || pathname === '/video/generations' || pathname === '/videos/generations')) {
            return create('video', req, res);
        }
        if (req.method === 'POST' && pathname === '/images/generations') return create('image', req, res);
        if (req.method === 'POST' && pathname === '/audio/generations') return create('audio', req, res);

        const match = /^\/(videos|images\/generations|audio\/generations)\/([^/]+)(?:\/(content|cancel))?$/.exec(pathname);
        if (!match) return false;
        const kind = match[1] === 'videos' ? 'video' : match[1].startsWith('images') ? 'image' : 'audio';
        const [, , id, action] = match;
        if (!action && req.method === 'GET') {
            const job = mediaManager.getJob(id);
            if (!job || job.kind !== kind) {
                sendApiError(res, { code: ERROR_CODES.INVALID_MODEL, message: '媒体任务不存在', status: 404 });
                return true;
            }
            sendJson(res, 200, mediaManager.formatJob(job));
            return true;
        }
        if (action === 'content' && (req.method === 'GET' || req.method === 'HEAD')) {
            await serveContent(req, res, mediaManager, id, kind);
            return true;
        }
        if (action === 'cancel' && req.method === 'POST') {
            const job = await mediaManager.cancelJob(id);
            if (!job || job.kind !== kind) {
                sendApiError(res, { code: ERROR_CODES.INVALID_MODEL, message: '媒体任务不存在', status: 404 });
                return true;
            }
            sendJson(res, 200, mediaManager.formatJob(job));
            return true;
        }
        return false;
    };
}
