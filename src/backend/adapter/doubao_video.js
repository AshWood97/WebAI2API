/**
 * Doubao Web video adapter.
 *
 * Requests are made from the already logged-in Camoufox page so Doubao's web
 * fetch interceptor, cookies and browser fingerprint remain in one place. It
 * intentionally does not run a second Playwright/Chromium process.
 */

import crypto from 'crypto';
import { safeClick, sleep, uploadFilesViaChooser } from '../engine/utils.js';
import { gotoWithCheck } from '../utils/index.js';
import { logger } from '../../utils/logger.js';
import { DOUBAO_PROVIDER, DOUBAO_VIDEO_PARAMETERS } from './doubao_catalog.js';

const TARGET_URL = 'https://www.doubao.com/chat/';
const VIDEO_URL_PATTERN = /(?:\.mp4|\.m3u8|\/video\/|douyinvod|video_gen|mime_type=video_mp4)/i;

export function asJson(value) {
    if (typeof value !== 'string') return value;
    try { return JSON.parse(value); } catch { return value; }
}

export function parseSse(raw) {
    const events = [];
    for (const block of String(raw || '').split(/\n\n/)) {
        const dataLine = block.split('\n').find(line => line.startsWith('data:'));
        if (!dataLine) continue;
        try {
            events.push(JSON.parse(dataLine.slice(5).trim()));
        } catch { /* Ignore heartbeat and non-JSON lines. */ }
    }
    return events;
}

function findId(value, terms) {
    value = asJson(value);
    if (Array.isArray(value)) {
        for (const item of value) {
            const found = findId(item, terms);
            if (found) return found;
        }
        return '';
    }
    if (!value || typeof value !== 'object') return '';
    for (const [key, candidate] of Object.entries(value)) {
        const compact = key.replace(/[-_\s]/g, '').toLowerCase();
        if (compact.includes('id') && terms.some(term => compact.includes(term)) && candidate && String(candidate) !== '0') {
            return String(candidate);
        }
    }
    for (const child of Object.values(value)) {
        const found = findId(child, terms);
        if (found) return found;
    }
    return '';
}

export function decodePossibleUrl(value) {
    if (typeof value !== 'string') return '';
    if (/^https?:\/\//i.test(value)) return value;
    try {
        const decoded = Buffer.from(value, 'base64').toString('utf8');
        return /^https?:\/\//i.test(decoded) ? decoded : '';
    } catch {
        return '';
    }
}

function collectResponse(raw) {
    const videos = [];
    const texts = [];
    let providerTaskId = '';
    let conversationId = '';
    const seen = new Set();

    const addVideo = (value, details = {}) => {
        const url = decodePossibleUrl(value);
        if (!url || !VIDEO_URL_PATTERN.test(url) || seen.has(url)) return;
        seen.add(url);
        videos.push({
            video_url: url,
            cover_url: details.cover_url || details.poster || details.cover?.url || '',
            width: details.width || details.video_width || 0,
            height: details.height || details.video_height || 0,
            duration: details.duration || details.video_duration || 0
        });
    };
    const walk = value => {
        value = asJson(value);
        if (Array.isArray(value)) {
            value.forEach(walk);
            return;
        }
        if (!value || typeof value !== 'object') return;
        if (!conversationId) conversationId = findId(value, ['conversation', 'thread']);
        if (!providerTaskId) providerTaskId = findId(value, ['task']);
        const content = asJson(value.content);
        if (content && typeof content === 'object' && typeof content.text === 'string') texts.push(content.text);
        if (value.text_block?.text) texts.push(value.text_block.text);
        for (const key of ['video_url', 'url', 'main_url', 'play_url', 'download_url']) addVideo(value[key], value);
        if (value.video_model) walk(value.video_model);
        for (const child of Object.values(value)) {
            if (child !== content && child !== value.video_model) walk(child);
        }
    };

    for (const event of parseSse(raw)) {
        if (!conversationId) conversationId = findId(event, ['conversation', 'thread']);
        const eventData = asJson(event.event_data);
        if (event.event_type === 2005) {
            const message = typeof eventData === 'string' ? eventData : JSON.stringify(eventData || {});
            return { error: `豆包视频请求失败: ${message.slice(0, 500)}` };
        }
        if (eventData?.fin_reason?.async_task?.id) providerTaskId = String(eventData.fin_reason.async_task.id);
        walk(eventData || event);
    }
    return {
        videos,
        providerTaskId,
        conversationId,
        message: texts.join('').trim()
    };
}

function isTerminalFailure(message) {
    const text = String(message || '').toLowerCase();
    return [
        '额度不足', '额度已用完', '没有视频生成权益', '免费次数已用完',
        '积分不足', '余额不足', 'quota exceeded', 'quota exhausted', 'captcha'
    ].some(marker => text.includes(marker));
}

function buildQuery(params) {
    return new URLSearchParams({
        aid: '497858',
        device_id: params.deviceId || '',
        device_platform: 'web',
        fp: params.fp || '',
        language: 'zh',
        pc_version: '3.22.5',
        pkg_type: 'release_version',
        real_aid: '497858',
        region: params.region || '',
        samantha_web: '1',
        sys_region: params.region || '',
        tea_uuid: params.webId || '',
        'use-olympus-account': '1',
        version_code: '20800',
        web_id: params.webId || '',
        web_platform: 'browser',
        web_tab_id: crypto.randomUUID()
    }).toString();
}

async function getRuntimeParams(page) {
    return await page.evaluate(() => {
        const readJson = key => {
            try { return JSON.parse(localStorage.getItem(key) || '{}'); } catch { return {}; }
        };
        const sam = readJson('samantha_web_web_id');
        const tea = readJson('__tea_cache_tokens_497858');
        const fp = document.cookie.split(';').map(item => item.trim()).find(item => item.startsWith('s_v_web_id='));
        return {
            deviceId: sam.web_id || '',
            webId: tea.web_id || '',
            fp: fp ? fp.slice('s_v_web_id='.length) : '',
            region: localStorage.getItem('flow_user_country') || ''
        };
    });
}

export async function browserSsePost(page, endpoint, payload, timeoutMs) {
    const runtime = await getRuntimeParams(page);
    if (!runtime.deviceId || !runtime.webId) throw new Error('豆包会话参数不可用，请重新登录豆包网页');
    const url = `${endpoint}?${buildQuery(runtime)}`;
    const result = await page.evaluate(async ({ url, payloadJson, timeoutMs }) => {
        const csrf = document.cookie.match(/passport_csrf_token=([^;]+)/)?.[1] || '';
        const controller = new AbortController();
        const timer = setTimeout(() => controller.abort(), timeoutMs);
        try {
            const response = await fetch(url, {
                method: 'POST',
                credentials: 'include',
                headers: {
                    Accept: 'text/event-stream',
                    'Content-Type': 'application/json',
                    'Agw-Js-Conv': 'str',
                    ...(csrf ? { 'x-tt-passport-csrf-token': csrf } : {})
                },
                body: payloadJson,
                signal: controller.signal
            });
            const body = await response.text();
            return { ok: response.ok, status: response.status, body };
        } catch (error) {
            return { ok: false, status: 0, body: String(error?.message || error) };
        } finally {
            clearTimeout(timer);
        }
    }, { url, payloadJson: JSON.stringify(payload), timeoutMs });
    if (!result.ok) throw new Error(`豆包网页接口失败 (${result.status}): ${String(result.body).slice(0, 500)}`);
    return result.body;
}

async function uploadReferences(page, inputPaths, meta) {
    if (!inputPaths?.length) return [];
    const storeUris = new Set();
    const responseHandler = async response => {
        try {
            if (!response.url().includes('Action=ApplyImageUpload') || response.status() !== 200) return;
            const body = await response.json();
            const uri = body?.Result?.UploadAddress?.StoreInfos?.[0]?.StoreUri;
            if (uri) storeUris.add(uri);
        } catch { /* A non-JSON upload response is not a usable reference. */ }
    };
    page.on('response', responseHandler);
    try {
        const menuButton = page.locator('#input-engine-container button[aria-haspopup="menu"]')
            .filter({ hasNot: page.locator('text=/Fast|Think|Pro|快速|思考|专家|更多/') }).first();
        await safeClick(page, menuButton, { bias: 'button', timeout: 10_000 });
        await sleep(300, 500);
        const uploadItem = page.locator('div[role="menuitem"]').filter({ hasText: /上传文件或图片|上傳檔案或圖片|Upload File or Image/ }).first();
        await uploadFilesViaChooser(page, uploadItem, inputPaths, {
            uploadValidator: response => response.status() === 200 && response.url().includes('ApplyImageUpload')
        }, meta);
        await sleep(500, 1000);
    } finally {
        page.off('response', responseHandler);
    }
    if (storeUris.size !== inputPaths.length) {
        throw new Error('豆包参考图上传未完成；请确认网页帐号具有上传权限后重试');
    }
    return [...storeUris];
}

function buildVideoPayload(prompt, options, imageKeys) {
    const videoModel = options?.provider_options?.video_model || 'seedance_v2.0';
    const content = {
        text: prompt,
        model: videoModel,
        duration: options?.seconds || 10
    };
    if (options?.size) content.ratio = options.size;
    if (imageKeys.length) {
        content.ref_image_key = imageKeys[0];
        content.ref_image_keys = imageKeys;
        content.reference_image_keys = imageKeys;
        content.reference_images = imageKeys.map(key => ({ key, type: 'image' }));
    }
    const message = {
        content: JSON.stringify(content),
        content_type: 2020,
        attachments: imageKeys.map(key => ({ type: 'image', key, extra: { refer_types: 'overall' } })),
        references: [],
        skill: { skill_type: 17, skill_type_no_default: 17, skill_id: '17', skill_id_no_default: '17' }
    };
    return {
        messages: [message],
        completion_option: {
            is_regen: false,
            with_suggest: true,
            need_create_conversation: true,
            launch_stage: 1,
            is_replace: false,
            is_delete: false,
            is_ai_playground: false,
            memory_type: 2,
            message_from: 0,
            use_deep_think: false,
            use_auto_cot: false,
            resend_for_regen: false,
            enable_commerce_credit: false,
            action_bar_skill_id: 17,
            conversation_init_option: { need_ack_conversation: true }
        },
        evaluate_option: { web_ab_params: '' },
        ext: { conversation_init_option: JSON.stringify({ need_ack_conversation: true }) },
        local_conversation_id: crypto.randomUUID(),
        local_message_id: crypto.randomUUID()
    };
}

export async function createVideo(context, payload, modelId, meta = {}) {
    const { page } = context;
    try {
        await gotoWithCheck(page, TARGET_URL);
        const imageKeys = await uploadReferences(page, payload.inputPaths || [], meta);
        const raw = await browserSsePost(page, '/samantha/chat/completion', buildVideoPayload(payload.prompt, payload.options, imageKeys), 60_000);
        const parsed = collectResponse(raw);
        if (parsed.error) return parsed;
        if (parsed.videos.length) return { outputs: parsed.videos, message: parsed.message };
        if (isTerminalFailure(parsed.message)) return { error: parsed.message || '豆包视频额度或权限不可用', retryable: false };
        if (!parsed.providerTaskId && !parsed.conversationId) {
            return { error: `豆包未返回可恢复的视频任务标识: ${(parsed.message || 'unknown').slice(0, 300)}` };
        }
        return {
            pending: true,
            progress: 30,
            providerTaskId: parsed.providerTaskId,
            conversationId: parsed.conversationId,
            message: parsed.message || '豆包已接受视频生成任务'
        };
    } catch (error) {
        logger.error('适配器', '豆包视频提交失败', { ...meta, error: error.message, modelId });
        return { error: `豆包视频提交失败: ${error.message}` };
    }
}

export async function pollVideo(context, payload, modelId, meta = {}) {
    const { page } = context;
    const job = payload.job || {};
    try {
        if (job.provider_task_id) {
            const raw = await browserSsePost(page, '/samantha/chat/async/stream', {
                task_id: job.provider_task_id,
                event_id: 0
            }, 45_000);
            const parsed = collectResponse(raw);
            if (parsed.error) return parsed;
            if (parsed.videos.length) return { outputs: parsed.videos, message: parsed.message };
            if (isTerminalFailure(parsed.message)) return { error: parsed.message || '豆包视频任务失败', retryable: false };
            return { pending: true, progress: 45, message: parsed.message || job.message };
        }
        if (job.conversation_id) {
            await gotoWithCheck(page, `https://www.doubao.com/chat/${job.conversation_id}`);
            const urls = await page.evaluate(() => {
                const pattern = /mp4|m3u8|douyinvod|mime_type=video_mp4|video_gen/i;
                const found = new Set();
                for (const element of document.querySelectorAll('video, source, a, [src], [href]')) {
                    for (const key of ['src', 'href', 'currentSrc']) {
                        const value = element[key] || element.getAttribute?.(key);
                        if (typeof value === 'string' && /^https?:/.test(value) && pattern.test(value)) found.add(value);
                    }
                }
                return [...found];
            });
            if (urls.length) return { outputs: urls.map(video_url => ({ video_url })), message: '视频已生成' };
        }
        return { pending: true, progress: 45, message: job.message || '豆包仍在生成视频' };
    } catch (error) {
        logger.warn('适配器', '豆包视频轮询失败', { ...meta, error: error.message, modelId });
        return { pending: true, progress: 35, message: `轮询暂时失败，将自动重试：${error.message}` };
    }
}

export async function cancelVideo() {
    // Doubao Web has no stable public cancel endpoint. Marking the local task as
    // cancelled prevents further polling without guessing an upstream mutation.
    return { cancelled: true };
}

async function generate() {
    return { error: '视频模型请使用 POST /v1/videos' };
}

export const manifest = {
    id: 'doubao_video',
    displayName: '豆包视频（Seedance）',
    provider: DOUBAO_PROVIDER.id,
    providerName: DOUBAO_PROVIDER.displayName,
    description: '复用已登录的豆包 Web 页面创建和轮询 Seedance 视频任务。',
    getTargetUrl() { return TARGET_URL; },
    models: [
        { id: 'doubao-video', displayName: '豆包视频（Seedance 2.0）', imagePolicy: 'optional', type: 'video', capabilities: ['text_to_video', 'image_to_video', 'async'], webParameters: DOUBAO_VIDEO_PARAMETERS },
        { id: 'seedance-2.0', displayName: 'Seedance 2.0', imagePolicy: 'optional', type: 'video', capabilities: ['text_to_video', 'image_to_video', 'async'], webParameters: DOUBAO_VIDEO_PARAMETERS },
        { id: 'seedance-2.0-fast', displayName: 'Seedance 2.0 Fast', imagePolicy: 'optional', type: 'video', capabilities: ['text_to_video', 'image_to_video', 'async'], webParameters: DOUBAO_VIDEO_PARAMETERS }
    ],
    navigationHandlers: [],
    generate,
    createVideo,
    pollVideo,
    cancelVideo
};
