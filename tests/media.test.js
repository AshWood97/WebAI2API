import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { execFile as execFileCallback } from 'node:child_process';
import { promisify } from 'node:util';

import { MediaManager } from '../src/media/index.js';
import { Worker } from '../src/backend/pool/Worker.js';
import { createMediaRouter } from '../src/server/api/openai/mediaRoutes.js';
import { parseMusicResult } from '../src/backend/adapter/doubao_audio.js';
import { manifest as doubaoTextManifest, parseDoubaoTranscriptionResponse } from '../src/backend/adapter/doubao_text.js';
import { manifest as doubaoManifest } from '../src/backend/adapter/doubao.js';
import { registry } from '../src/backend/registry.js';
import { parseRequest } from '../src/server/api/openai/parse.js';

async function tempDataDir(t) {
    const dir = await fs.mkdtemp(path.join(os.tmpdir(), 'webai-media-test-'));
    t.after(async () => fs.rm(dir, { recursive: true, force: true }));
    return dir;
}

const logger = { info() {}, warn() {}, error() {}, debug() {} };
const execFile = promisify(execFileCallback);

test('media jobs are idempotent and persist data-url output outside SQLite', async t => {
    const manager = new MediaManager({
        dataDir: await tempDataDir(t), logger,
        mediaConfig: { pollIntervalMs: 5, pollTimeoutMs: 500, maxBytes: 1024 * 1024 },
        getWorkerCookies: async () => ({ cookies: [] }),
        executeMedia: async () => ({ image: 'data:image/png;base64,aGVsbG8=' })
    });
    const first = manager.createJob('image', { model: 'image-test', prompt: 'hello' }, { idempotencyKey: 'same-request' });
    const second = manager.createJob('image', { model: 'image-test', prompt: 'hello' }, { idempotencyKey: 'same-request' });
    assert.equal(second.reused, true);
    assert.equal(second.job.id, first.job.id);
    await manager.running.get(first.job.id);
    const job = manager.getJob(first.job.id);
    assert.equal(job.status, 'completed');
    assert.equal(job.outputs.length, 1);
    assert.equal((await fs.readFile(manager.getContent(job).absolutePath)).toString(), 'hello');
});

test('media runtime status blocks deployment while a job is running', async t => {
    let release;
    const manager = new MediaManager({
        dataDir: await tempDataDir(t), logger,
        mediaConfig: { pollIntervalMs: 5, pollTimeoutMs: 500 },
        getWorkerCookies: async () => ({ cookies: [] }),
        executeMedia: async () => await new Promise(resolve => { release = resolve; })
    });
    const { job } = manager.createJob('image', { model: 'image-test', prompt: 'hold' });
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(manager.getRuntimeStatus().idle, false);
    release({ image: 'data:image/png;base64,aGVsbG8=' });
    await manager.running.get(job.id);
    assert.deepEqual(manager.getRuntimeStatus(), {
        runningCount: 0,
        recoverableCount: 0,
        recoverableJobIds: [],
        idle: true
    });
});

test('pending video jobs poll to completion without duplicating submit', async t => {
    let creates = 0;
    let polls = 0;
    const manager = new MediaManager({
        dataDir: await tempDataDir(t), logger,
        mediaConfig: { pollIntervalMs: 5, pollTimeoutMs: 500, maxBytes: 1024 * 1024 },
        getWorkerCookies: async () => ({ cookies: [] }),
        executeMedia: async (_kind, payload) => {
            if (payload.operation === 'create') {
                creates++;
                return { pending: true, providerTaskId: 'provider-task', conversationId: 'conversation' };
            }
            polls++;
            return { outputs: [{ video_url: 'data:video/mp4;base64,dmlkZW8=' }] };
        }
    });
    const { job } = manager.createJob('video', { model: 'video-test', prompt: 'animate' });
    await manager.running.get(job.id);
    const completed = manager.getJob(job.id);
    assert.equal(creates, 1);
    assert.equal(polls, 1);
    assert.equal(completed.status, 'completed');
    assert.equal(completed.provider_task_id, 'provider-task');
});

test('worker page lock serializes tasks', async () => {
    const worker = new Worker({ backend: { pool: {} } }, {
        name: 'lock-test', type: 'test', userDataDir: '/tmp/unused', resolvedProxy: null
    });
    const order = [];
    const first = worker._withPageLock(async () => {
        order.push('first-start');
        await new Promise(resolve => setTimeout(resolve, 20));
        order.push('first-end');
    });
    const second = worker._withPageLock(async () => order.push('second'));
    await Promise.all([first, second]);
    assert.deepEqual(order, ['first-start', 'first-end', 'second']);
});

test('video API returns an async job and serves byte ranges after completion', async t => {
    const manager = new MediaManager({
        dataDir: await tempDataDir(t), logger,
        mediaConfig: { pollIntervalMs: 5, pollTimeoutMs: 500, maxBytes: 1024 * 1024 },
        getWorkerCookies: async () => ({ cookies: [] }),
        executeMedia: async (_kind, payload) => payload.operation === 'create'
            ? { pending: true, providerTaskId: 'provider-task' }
            : { outputs: [{ video_url: 'data:video/mp4;base64,dmlkZW8=' }] }
    });
    const route = createMediaRouter({
        getModels: () => ({ object: 'list', data: [{ id: 'doubao-video', type: 'video' }] }),
        mediaManager: manager
    });
    const server = http.createServer(async (req, res) => {
        const handled = await route(req, res, new URL(req.url, 'http://localhost').pathname);
        if (handled === false) res.writeHead(404).end();
    });
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
    t.after(() => server.close());
    const baseUrl = `http://127.0.0.1:${server.address().port}`;
    const created = await fetch(`${baseUrl}/videos`, {
        method: 'POST', headers: { 'content-type': 'application/json', 'idempotency-key': 'job-key' },
        body: JSON.stringify({ model: 'doubao-video', prompt: 'animate a cube', seconds: 5 })
    });
    assert.equal(created.status, 202);
    const job = await created.json();
    await manager.running.get(job.id);
    const status = await (await fetch(`${baseUrl}/videos/${job.id}`)).json();
    assert.equal(status.status, 'completed');
    const content = await fetch(`${baseUrl}/videos/${job.id}/content`, { headers: { range: 'bytes=1-3' } });
    assert.equal(content.status, 206);
    assert.equal(await content.text(), 'ide');
});

test('enable script adds the worker once without rewriting existing settings', async t => {
    const dir = await tempDataDir(t);
    const configPath = path.join(dir, 'config.yaml');
    await fs.writeFile(configPath, `server:\n  auth: keep-me\nbackend:\n  pool:\n    instances:\n      - name: browser_default\n        workers:\n          - name: doubao\n            type: doubao\n          - name: doubao_text\n            type: doubao_text\n          - name: chatgpt\n            type: chatgpt\n`);
    const script = path.join(process.cwd(), 'scripts', 'enable-doubao-video.mjs');
    await execFile(process.execPath, [script, configPath]);
    await execFile(process.execPath, [script, configPath]);
    const updated = await fs.readFile(configPath, 'utf8');
    assert.equal(updated.includes('auth: keep-me'), true);
    assert.equal((updated.match(/^\s*- name: "doubao_video"\s*$/gm) || []).length, 1);
    assert.equal(updated.includes('          - name: chatgpt'), true);
    assert.equal((updated.match(/^media:/gm) || []).length, 1);
});

test('Doubao music parser extracts a base64-encoded audio URL from SSE', () => {
    const url = 'https://cdn.example.test/song.mp3';
    const content = {
        tasks: {
            task: {
                title: 'Song',
                video_model: JSON.stringify({
                    video_duration: 12,
                    video_list: { normal: { main_url: Buffer.from(url).toString('base64') } }
                })
            }
        }
    };
    const eventData = { message: { content_type: 2006, content: JSON.stringify(content) } };
    const parsed = parseMusicResult(`data: ${JSON.stringify({ event_type: 2001, event_data: JSON.stringify(eventData) })}\n\n`);
    assert.equal(parsed.tracks.length, 1);
    assert.equal(parsed.tracks[0].audio_url, url);
    assert.equal(parsed.tracks[0].duration, 12);
});

test('Doubao transcription parser extracts JSON and SSE text without returning raw SSE', () => {
    assert.deepEqual(parseDoubaoTranscriptionResponse({ transcript: 'hello from JSON' }), { text: 'hello from JSON' });
    assert.deepEqual(
        parseDoubaoTranscriptionResponse('event: message\ndata: {"result":{"transcript":"hello from SSE"}}\n\ndata: [DONE]\n'),
        { text: 'hello from SSE' }
    );
    assert.deepEqual(
        parseDoubaoTranscriptionResponse('event: message\ndata: {"progress":42}\n\ndata: [DONE]\n'),
        { text: '' }
    );
});

test('Doubao text manifest exposes the verified webpage capabilities', () => {
    const models = new Map(doubaoTextManifest.models.map(model => [model.id, model]));
    assert.deepEqual(models.get('seed-pro').capabilities, ['expert_mode']);
    assert.deepEqual(models.get('doubao-work-task-turbo').capabilities, ['work_task_turbo']);
    assert.deepEqual(models.get('doubao-deep-research').capabilities, ['deep_research']);
    assert.deepEqual(models.get('doubao-transcription').capabilities, ['audio_transcription']);
    assert.equal(models.get('doubao-transcription').type, 'transcription');
});

test('Doubao catalogue records verified web controls without inventing a resolution', () => {
    const models = new Map(doubaoManifest.models.map(model => [model.id, model]));
    const imageRatio = models.get('seedream-4.5').webParameters.find(parameter => parameter.key === 'ratio');
    const videoRatio = models.get('seedance-2.0').webParameters.find(parameter => parameter.key === 'ratio');
    const videoDuration = models.get('seedance-2.0').webParameters.find(parameter => parameter.key === 'duration');
    const resolution = models.get('seedance-2.0').webParameters.find(parameter => parameter.key === 'resolution');

    assert.deepEqual(imageRatio.values, ['自动', '9:16', '2:3', '3:4', '1:1', '4:3', '3:2', '16:9']);
    assert.deepEqual(videoRatio.values, ['自动', '3:4', '4:3', '9:16', '16:9', '1:1', '21:9']);
    assert.deepEqual(videoDuration.values, ['4s', '10s', '15s']);
    assert.deepEqual(resolution.values, []);
    assert.match(resolution.note, /480p、720p/);
    assert.equal(models.has('seedream-5.0-pro'), true);
});

test('worker model entries use the configured account alias and keep provider metadata', async () => {
    await registry.loadAll();
    const worker = new Worker({ backend: { pool: {} } }, {
        name: 'doubao-primary',
        accountName: '豆包主账号',
        type: 'doubao',
        userDataDir: '/tmp/unused',
        resolvedProxy: null
    });
    const model = worker.getModels().find(item => item.id === 'doubao/seedream-4.5');
    assert.equal(model.account_id, '豆包主账号');
    assert.equal(model.account_name, '豆包主账号');
    assert.equal(model.provider, 'doubao');
    assert.equal(model.display_name, 'Seedream 4.5');
    assert.equal(model.web_parameters.some(parameter => parameter.key === 'ratio'), true);
});

test('chat completions reject transcription models with the dedicated endpoint', async () => {
    const parsed = await parseRequest({
        model: 'doubao-transcription',
        messages: [{ role: 'user', content: 'this must not become a chat request' }]
    }, {
        tempDir: os.tmpdir(),
        imageLimit: 1,
        backendName: 'pool',
        getSupportedModels: () => ({ object: 'list', data: [{ id: 'doubao-transcription' }] }),
        getImagePolicy: () => 'forbidden',
        getModelType: () => 'transcription',
        requestId: 'test',
        logger
    });
    assert.equal(parsed.success, false);
    assert.match(parsed.error.error, /POST \/v1\/audio\/transcriptions/);
});

async function startRoute(t, context) {
    const route = createMediaRouter(context);
    const server = http.createServer(async (req, res) => {
        const handled = await route(req, res, new URL(req.url, 'http://localhost').pathname);
        if (handled === false) res.writeHead(404).end();
    });
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
    t.after(() => server.close());
    return `http://127.0.0.1:${server.address().port}`;
}

function transcriptionForm({ model = 'doubao-transcription', responseFormat = 'json' } = {}) {
    const form = new FormData();
    form.set('model', model);
    form.set('response_format', responseFormat);
    form.set('file', new Blob(['not a real recording'], { type: 'audio/mpeg' }), 'recording.mp3');
    return form;
}

test('audio transcriptions accept multipart input and return each supported response format', async t => {
    const calls = [];
    const baseUrl = await startRoute(t, {
        getModels: () => ({ object: 'list', data: [{ id: 'doubao-transcription', type: 'transcription' }] }),
        mediaManager: {
            maxUploadBytes: 1024 * 1024,
            saveUpload: async upload => ({ ...upload, absolute_path: '/private/uploads/recording.mp3' })
        },
        transcribe: async (payload, model, meta) => {
            calls.push({ payload, model, meta });
            return { text: 'converted speech', language: 'zh', duration: 1.25 };
        }
    });

    const jsonResponse = await fetch(`${baseUrl}/audio/transcriptions`, {
        method: 'POST', body: transcriptionForm()
    });
    assert.equal(jsonResponse.status, 200);
    assert.deepEqual(await jsonResponse.json(), { text: 'converted speech' });

    const textResponse = await fetch(`${baseUrl}/audio/transcriptions`, {
        method: 'POST', body: transcriptionForm({ responseFormat: 'text' })
    });
    assert.equal(textResponse.status, 200);
    assert.equal(await textResponse.text(), 'converted speech');

    const verboseResponse = await fetch(`${baseUrl}/audio/transcriptions`, {
        method: 'POST', body: transcriptionForm({ responseFormat: 'verbose_json' })
    });
    assert.equal(verboseResponse.status, 200);
    assert.deepEqual(await verboseResponse.json(), {
        task: 'transcribe', language: 'zh', duration: 1.25, text: 'converted speech'
    });
    assert.equal(calls.length, 3);
    assert.equal(calls[0].model, 'doubao-transcription');
    assert.equal(calls[0].payload.filePath, '/private/uploads/recording.mp3');
    assert.equal(calls[0].payload.file.purpose, 'transcription');
});

test('audio transcriptions reject a non-transcription model and redact provider details', async t => {
    let transcribeCalls = 0;
    const baseUrl = await startRoute(t, {
        getModels: () => ({ object: 'list', data: [
            { id: 'seed', type: 'text' },
            { id: 'doubao-transcription', type: 'transcription' }
        ] }),
        mediaManager: {
            maxUploadBytes: 1024 * 1024,
            saveUpload: async upload => ({ ...upload, absolute_path: '/private/uploads/recording.mp3' })
        },
        transcribe: async () => {
            transcribeCalls++;
            return { error: 'provider failed at /private/uploads/recording.mp3 token=supersecret' };
        }
    });

    const invalidModel = await fetch(`${baseUrl}/audio/transcriptions`, {
        method: 'POST', body: transcriptionForm({ model: 'seed' })
    });
    assert.equal(invalidModel.status, 400);
    assert.match((await invalidModel.json()).error.message, /不能用于 transcription/);
    assert.equal(transcribeCalls, 0);

    const providerFailure = await fetch(`${baseUrl}/audio/transcriptions`, {
        method: 'POST', body: transcriptionForm()
    });
    assert.equal(providerFailure.status, 502);
    const response = await providerFailure.json();
    assert.doesNotMatch(response.error.message, /private\/uploads|supersecret/);
});
