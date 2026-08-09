/**
 * Provider-neutral asynchronous media job runner.
 */

import crypto from 'crypto';
import fs from 'fs';
import fsp from 'fs/promises';
import path from 'path';
import { Readable } from 'stream';
import { pipeline } from 'stream/promises';
import { JobStore } from './JobStore.js';

const DEFAULT_RETENTION_SECONDS = 24 * 60 * 60;
const DEFAULT_MAX_BYTES = 2 * 1024 * 1024 * 1024;
const DEFAULT_MAX_UPLOAD_BYTES = 20 * 1024 * 1024;

function safeFilename(name, fallback = 'media.bin') {
    const sanitized = String(name || fallback)
        .replace(/[^a-zA-Z0-9._-]/g, '_')
        .replace(/^\.+/, '')
        .slice(-120);
    return sanitized || fallback;
}

function extensionForMime(mime, kind) {
    if (mime?.includes('mp4')) return '.mp4';
    if (mime?.includes('webm')) return '.webm';
    if (mime?.includes('mpeg')) return '.mp3';
    if (mime?.includes('wav')) return '.wav';
    if (mime?.includes('png')) return '.png';
    if (mime?.includes('jpeg')) return '.jpg';
    return kind === 'video' ? '.mp4' : kind === 'audio' ? '.mp3' : '.bin';
}

function mimeForKind(kind) {
    return kind === 'video' ? 'video/mp4' : kind === 'audio' ? 'audio/mpeg' : 'image/png';
}

function dataUrlToBuffer(value) {
    const match = /^data:([^;,]+)?(;base64)?,(.*)$/s.exec(value || '');
    if (!match) return null;
    const mimeType = match[1] || 'application/octet-stream';
    const buffer = match[2]
        ? Buffer.from(match[3], 'base64')
        : Buffer.from(decodeURIComponent(match[3]), 'utf8');
    return { mimeType, buffer };
}

/**
 * @param {object} options
 * @param {string} options.dataDir
 * @param {(kind: string, payload: object, model: string, meta: object) => Promise<object>} options.executeMedia
 * @param {(workerName: string, domain?: string) => Promise<object>} options.getWorkerCookies
 * @param {object} [options.mediaConfig]
 * @param {object} options.logger
 */
export class MediaManager {
    constructor({ dataDir, executeMedia, getWorkerCookies, mediaConfig = {}, logger }) {
        this.dataDir = dataDir;
        this.executeMedia = executeMedia;
        this.getWorkerCookies = getWorkerCookies;
        this.logger = logger;
        this.store = new JobStore(dataDir);
        this.retentionSeconds = mediaConfig.retentionSeconds || DEFAULT_RETENTION_SECONDS;
        this.maxBytes = mediaConfig.maxBytes || DEFAULT_MAX_BYTES;
        this.maxUploadBytes = mediaConfig.maxUploadBytes || DEFAULT_MAX_UPLOAD_BYTES;
        this.pollIntervalMs = mediaConfig.pollIntervalMs || 10_000;
        this.pollTimeoutMs = mediaConfig.pollTimeoutMs || 45 * 60 * 1000;
        this.running = new Map();
        this.cleanupTimer = null;
    }

    start() {
        if (!this.cleanupTimer) {
            this.cleanupTimer = setInterval(() => this.cleanup().catch(() => {}), 60 * 60 * 1000);
            this.cleanupTimer.unref?.();
        }
        for (const job of this.store.listRecoverableJobs()) this.schedule(job.id, true);
    }

    stop() {
        if (this.cleanupTimer) clearInterval(this.cleanupTimer);
        this.cleanupTimer = null;
    }

    createJob(kind, request, { idempotencyKey, scope = 'default' } = {}) {
        const { job, reused } = this.store.createJob({
            kind,
            model: request.model,
            prompt: request.prompt,
            options: request.options || {},
            inputFileIds: request.inputFileIds || [],
            idempotencyKey,
            scope,
            retentionSeconds: this.retentionSeconds
        });
        if (!reused) this.schedule(job.id);
        return { job, reused };
    }

    getJob(id) {
        return this.store.getJob(id);
    }

    formatJob(job, basePath = '/v1') {
        if (!job) return null;
        const output = job.outputs[0] || null;
        const response = {
            id: job.id,
            object: job.kind === 'video' ? 'video' : `${job.kind}.generation`,
            created_at: job.created_at,
            updated_at: job.updated_at,
            status: job.status,
            progress: job.progress,
            model: job.model,
            prompt: job.prompt,
            error: job.error ? { message: job.error, type: 'provider_error', code: 'media_generation_failed' } : undefined,
            message: job.message || undefined
        };
        if (job.kind === 'video') {
            response.content_url = job.status === 'completed' && output ? `${basePath}/videos/${job.id}/content` : undefined;
            response.metadata = output ? { mime_type: output.mime_type, bytes: output.size_bytes } : undefined;
        } else if (output) {
            response.data = [{
                url: `${basePath}/${job.kind === 'image' ? 'images' : 'audio'}/generations/${job.id}/content`,
                mime_type: output.mime_type
            }];
        }
        return Object.fromEntries(Object.entries(response).filter(([, value]) => value !== undefined));
    }

    async cancelJob(id) {
        const job = this.store.getJob(id);
        if (!job) return null;
        if (this.store.isTerminal(job)) return job;
        this.store.markCancelled(id);
        if (job.worker_name) {
            this.executeMedia(job.kind, { operation: 'cancel', job }, job.model, { mediaJobId: job.id })
                .catch(error => this.logger.warn('媒体任务', `上游取消失败: ${error.message}`, { id }));
        }
        return this.store.getJob(id);
    }

    async saveUpload({ filename, mimeType, buffer, purpose = 'user_data' }) {
        if (!Buffer.isBuffer(buffer)) throw new Error('上传内容无效');
        if (buffer.length === 0) throw new Error('上传内容为空');
        if (buffer.length > this.maxUploadBytes) throw new Error(`上传文件超过 ${Math.floor(this.maxUploadBytes / 1024 / 1024)}MB 限制`);

        const storedName = `${crypto.randomUUID()}-${safeFilename(filename)}`;
        const relativePath = path.join('uploads', storedName);
        const outputPath = path.join(this.store.mediaDir, relativePath);
        await fsp.writeFile(outputPath, buffer, { mode: 0o600 });
        return this.store.createFile({
            filename: safeFilename(filename),
            purpose,
            mimeType: mimeType || 'application/octet-stream',
            sizeBytes: buffer.length,
            relativePath,
            retentionSeconds: this.retentionSeconds
        });
    }

    getContent(job) {
        if (!job?.outputs?.length) return null;
        const output = job.outputs[0];
        const filePath = path.join(this.store.mediaDir, output.relative_path || '');
        if (!output.relative_path || !filePath.startsWith(this.store.mediaDir) || !fs.existsSync(filePath)) return null;
        return { ...output, absolutePath: filePath };
    }

    schedule(id, recovering = false) {
        if (this.running.has(id)) return this.running.get(id);
        const running = this._run(id, recovering)
            .catch(error => this.logger.error('媒体任务', `任务未处理异常: ${error.message}`, { id }))
            .finally(() => this.running.delete(id));
        this.running.set(id, running);
        return running;
    }

    async _run(id, recovering) {
        let job = this.store.getJob(id);
        if (!job || job.status === 'cancelled' || this.store.isTerminal(job)) return;

        try {
            if (recovering && job.provider_task_id) {
                this.logger.info('媒体任务', '恢复上游媒体任务', { id });
                await this._pollUntilTerminal(job);
                return;
            }

            this.store.updateJob(id, { status: 'in_progress', progress: 5, message: 'Submitting provider request' });
            job = this.store.getJob(id);
            const inputFiles = this.store.listFiles(job.input_file_ids);
            if (inputFiles.length !== job.input_file_ids.length) {
                throw new Error('一个或多个引用文件已过期或不存在');
            }
            const result = await this.executeMedia(job.kind, {
                operation: 'create',
                job,
                prompt: job.prompt,
                options: job.options,
                inputPaths: inputFiles.map(file => file.absolute_path),
                inputFiles
            }, job.model, { mediaJobId: job.id });
            await this._applyProviderResult(id, result, { submitting: true });
        } catch (error) {
            this.store.updateJob(id, { status: 'failed', progress: 100, error: error.message });
            this.logger.warn('媒体任务', `任务失败: ${error.message}`, { id });
        }
    }

    async _pollUntilTerminal(initialJob) {
        const startedAt = Date.now();
        let job = initialJob;
        while (Date.now() - startedAt < this.pollTimeoutMs) {
            job = this.store.getJob(job.id);
            if (!job || this.store.isTerminal(job)) return;
            if (job.status === 'cancelled') return;

            const result = await this.executeMedia(job.kind, {
                operation: 'poll',
                job,
                prompt: job.prompt,
                options: job.options
            }, job.model, { mediaJobId: job.id });
            const terminal = await this._applyProviderResult(job.id, result, { submitting: false });
            if (terminal) return;
            await new Promise(resolve => setTimeout(resolve, this.pollIntervalMs));
        }
        this.store.updateJob(job.id, {
            status: 'failed',
            progress: 100,
            error: '媒体任务轮询超时；上游任务可能仍在生成，可使用新的任务 ID 重试。'
        });
    }

    async _applyProviderResult(id, result, { submitting }) {
        const current = this.store.getJob(id);
        if (!current || current.status === 'cancelled') return true;
        if (result?.error) throw new Error(result.error);

        const changes = {
            status: result?.pending ? 'in_progress' : 'completed',
            progress: result?.pending ? Math.max(current.progress, result.progress || 30) : 90,
            provider: result?.adapter || current.provider,
            workerName: result?.workerName || current.worker_name,
            providerTaskId: result?.providerTaskId || current.provider_task_id,
            conversationId: result?.conversationId || current.conversation_id,
            message: result?.message || (result?.pending ? 'Provider accepted the request' : current.message)
        };
        this.store.updateJob(id, changes);

        if (result?.pending) {
            if (submitting && !result.providerTaskId && !result.conversationId) {
                throw new Error('上游已接受视频但未返回可恢复的任务或会话标识');
            }
            if (submitting) {
                await this._pollUntilTerminal(this.store.getJob(id));
                return true;
            }
            return false;
        }

        const outputs = await this._persistOutputs(this.store.getJob(id), result || {});
        if (outputs.length === 0) throw new Error('上游响应未包含可下载的媒体内容');
        this.store.updateJob(id, { status: 'completed', progress: 100, outputs, error: null, message: result.message || 'Completed' });
        return true;
    }

    async _persistOutputs(job, result) {
        const candidates = result.outputs || result.videos || result.tracks || [];
        const normalized = [...candidates];
        if (result.image) normalized.push({ url: result.image, mimeType: 'image/png' });
        if (result.audio) normalized.push({ url: result.audio, mimeType: 'audio/mpeg' });
        const outputs = [];
        for (const candidate of normalized.slice(0, 4)) {
            const url = typeof candidate === 'string'
                ? candidate
                : candidate.url || candidate.video_url || candidate.audio_url || candidate.download_url;
            if (!url) continue;
            const mimeType = candidate.mimeType || candidate.mime_type || mimeForKind(job.kind);
            outputs.push(await this._downloadOutput(job, url, mimeType));
        }
        return outputs;
    }

    async _downloadOutput(job, url, fallbackMimeType) {
        const data = dataUrlToBuffer(url);
        const extension = extensionForMime(data?.mimeType || fallbackMimeType, job.kind);
        const relativePath = path.join('outputs', `${job.id}-${crypto.randomUUID()}${extension}`);
        const outputPath = path.join(this.store.mediaDir, relativePath);
        if (data) {
            if (data.buffer.length > this.maxBytes) throw new Error('媒体输出超过容量限制');
            await fsp.writeFile(outputPath, data.buffer, { mode: 0o600 });
            return { relative_path: relativePath, mime_type: data.mimeType, size_bytes: data.buffer.length };
        }

        const controller = new AbortController();
        const timeout = setTimeout(() => controller.abort(), 10 * 60 * 1000);
        const headers = {};
        if (job.worker_name) {
            try {
                const domain = new URL(url).hostname;
                const cookieResult = await this.getWorkerCookies(job.worker_name, domain);
                if (cookieResult?.cookies?.length) {
                    headers.Cookie = cookieResult.cookies.map(cookie => `${cookie.name}=${cookie.value}`).join('; ');
                }
            } catch (error) {
                this.logger.debug('媒体任务', `下载时无法读取浏览器 Cookie: ${error.message}`, { id: job.id });
            }
        }
        try {
            const response = await fetch(url, { headers, redirect: 'follow', signal: controller.signal });
            if (!response.ok || !response.body) throw new Error(`媒体下载失败: HTTP ${response.status}`);
            const declaredLength = Number(response.headers.get('content-length') || 0);
            if (declaredLength > this.maxBytes) throw new Error('媒体输出超过容量限制');
            await pipeline(Readable.fromWeb(response.body), fs.createWriteStream(outputPath, { mode: 0o600 }));
            const stats = await fsp.stat(outputPath);
            if (stats.size > this.maxBytes) {
                await fsp.unlink(outputPath).catch(() => {});
                throw new Error('媒体输出超过容量限制');
            }
            return {
                relative_path: relativePath,
                mime_type: response.headers.get('content-type')?.split(';')[0] || fallbackMimeType,
                size_bytes: stats.size
            };
        } finally {
            clearTimeout(timeout);
        }
    }

    async cleanup() {
        const paths = this.store.collectExpiredPaths();
        for (const filePath of paths) await fsp.unlink(filePath).catch(() => {});
        let bytes = this.store.getStoredBytes();
        if (bytes <= this.maxBytes) return;
        // Existing files are already covered by expiry. If an operator lowers the
        // cap, leave live jobs intact and report rather than deleting active work.
        this.logger.warn('媒体任务', `媒体目录超过容量上限: ${bytes}/${this.maxBytes}`);
    }
}
