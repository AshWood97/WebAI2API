/**
 * @fileoverview 豆包 (Doubao) 文本生成适配器
 */

import {
    sleep,
    humanType,
    safeClick,
    uploadFilesViaChooser
} from '../engine/utils.js';
import {
    normalizePageError,
    waitForInput,
    gotoWithCheck
} from '../utils/index.js';
import { logger } from '../../utils/logger.js';

// --- 配置常量 ---
const TARGET_URL = 'https://www.doubao.com/chat/';

const TEXT_MODELS = Object.freeze({
    'seed': { kind: 'model', menu: /Fast Solves most questions|快速 适用于大部分情况|快速 適用於大部分情況/ },
    'seed-thinking': { kind: 'model', menu: /Think Solves more complex problems|思考 擅长解决更难的问题|思考 擅長解決更難的問題/ },
    'seed-pro': { kind: 'model', menu: /Pro Advanced Pro model|专家 研究级智能模型|專家 研究級智慧模型/ },
    'doubao-work-task-turbo': { kind: 'work_task_turbo' },
    'doubao-deep-research': { kind: 'deep_research' }
});

const TRANSCRIPTION_MODEL_ID = 'doubao-transcription';

async function firstVisibleLocator(locators, label) {
    let lastError;
    for (const locator of locators) {
        try {
            if (await locator.count() === 0) continue;
            await locator.first().waitFor({ state: 'visible', timeout: 5_000 });
            return locator.first();
        } catch (error) {
            lastError = error;
        }
    }
    throw new Error(`${label}不可用${lastError?.message ? `: ${lastError.message}` : ''}`);
}

async function clickFirstVisible(page, locators, meta, label) {
    const locator = await firstVisibleLocator(locators, label);
    await safeClick(page, locator, { bias: 'button' });
    return locator;
}

async function activateWorkTaskTurbo(page, meta) {
    await clickFirstVisible(page, [
        page.getByRole('button', { name: /^新工作任务$/i }),
        page.locator('[role="button"]').filter({ hasText: /^新工作任务$/i })
    ], meta, '豆包新工作任务入口');
    await sleep(600, 900);

    await clickFirstVisible(page, [
        page.locator('#input-engine-container button[aria-haspopup="menu"]')
            .filter({ hasText: /快速|专家|工作任务\s*(?:Turbo|Pro)/i }),
        page.loc('button[aria-haspopup="menu"]').filter({ hasText: /快速|专家|工作任务\s*(?:Turbo|Pro)/i })
    ], meta, '工作任务模式选择器');
    await clickFirstVisible(page, [
        page.getByRole('menuitem', { name: /工作任务\s*Turbo/i }),
        page.locator('[role="menuitem"]').filter({ hasText: /工作任务\s*Turbo/i })
    ], meta, '工作任务 Turbo');
    await page.getByRole('button', { name: /工作任务\s*Turbo/i }).first()
        .waitFor({ state: 'visible', timeout: 5_000 });
}

async function activateDeepResearch(page, meta) {
    await clickFirstVisible(page, [
        page.locator('#input-engine-container button').filter({ hasText: /^深入研究$/ }),
        page.getByRole('button', { name: /^深入研究$/i })
    ], meta, '深入研究入口');
    await page.locator('textarea[placeholder*="输入主题和报告要求"]').first()
        .waitFor({ state: 'visible', timeout: 5_000 });
}

async function selectTextMode(page, modelId, meta) {
    const mode = TEXT_MODELS[modelId] || TEXT_MODELS.seed;
    if (mode.kind === 'work_task_turbo') {
        await activateWorkTaskTurbo(page, meta);
        return;
    }
    if (mode.kind === 'deep_research') {
        await activateDeepResearch(page, meta);
        return;
    }

    const modelSelectorBtn = page.locator('#input-engine-container button[aria-haspopup="menu"]')
        .filter({ hasText: /Fast|Think|Pro|快速|思考|专家|專家/ })
        .first();
    let selectorExists = false;
    try {
        await modelSelectorBtn.waitFor({ state: 'attached', timeout: 5_000 });
        selectorExists = true;
    } catch {
        selectorExists = false;
    }
    if (!selectorExists) return;

    const menuItem = page.getByRole('menuitem', { name: mode.menu });
    for (let attempt = 1; attempt <= 3; attempt++) {
        await sleep(500, 1_000);
        await safeClick(page, modelSelectorBtn, { bias: 'button' });
        try {
            await menuItem.waitFor({ state: 'visible', timeout: 3_000 });
            break;
        } catch {
            logger.warn('适配器', `模型菜单未弹出，重试 ${attempt}/3`, meta);
            if (attempt === 3) throw new Error('模型选择菜单未弹出');
        }
    }
    await safeClick(page, menuItem, { bias: 'button' });
    await sleep(600, 1_000);
}

/**
 * 执行文本生成任务
 * @param {object} context - 浏览器上下文 { page, config }
 * @param {string} prompt - 提示词
 * @param {string[]} imgPaths - 图片路径数组
 * @param {string} [modelId] - 模型 ID
 * @param {object} [meta={}] - 日志元数据
 * @returns {Promise<{text?: string, reasoning?: string, error?: string}>}
 */
async function generate(context, prompt, imgPaths, modelId, meta = {}) {
    const { page, config } = context;
    const waitTimeout = config?.backend?.pool?.waitTimeout ?? 120000;

    const useThinking = ['seed-thinking', 'seed-pro', 'doubao-deep-research'].includes(modelId);

    try {
        logger.info('适配器', '开启新会话...', meta);
        await gotoWithCheck(page, TARGET_URL);

        // 1. 等待输入框加载
        const inputLocator = page.locator('textarea.semi-input-textarea');
        await waitForInput(page, inputLocator, { click: false });

        // 2. 选择普通模型，或激活网页的 Turbo / 深入研究模式。
        logger.debug('适配器', `选择豆包文本模式: ${modelId}`, meta);
        await sleep(300, 500);
        await selectTextMode(page, modelId, meta);

        // 3. 上传图片 (如果有)
        if (imgPaths && imgPaths.length > 0) {
            logger.info('适配器', `开始上传 ${imgPaths.length} 张图片...`, meta);

            // 预先拦截 ApplyImageUpload 响应，动态收集实际上传路径
            const expectedUploadPaths = new Set();
            const applyUploadHandler = async (response) => {
                try {
                    const url = response.url();
                    if (!url.includes('Action=ApplyImageUpload') || response.status() !== 200) return;
                    const json = await response.json();
                    const storeUri = json.Result?.UploadAddress?.StoreInfos?.[0]?.StoreUri;
                    if (storeUri) {
                        expectedUploadPaths.add(storeUri);
                        logger.debug('适配器', `已获取上传路径: ${storeUri}`, meta);
                    }
                } catch { /* 忽略解析错误 */ }
            };
            page.on('response', applyUploadHandler);

            try {
                // 点击上传菜单按钮（排除掉含有模型名称或带有“更多”文案的按钮）
                const uploadMenuBtn = page.locator('#input-engine-container button[aria-haspopup="menu"]')
                    .filter({ hasNot: page.locator('text=/Fast|Think|Pro|快速|思考|专家|專家|更多/') })
                    .first();
                await safeClick(page, uploadMenuBtn, { bias: 'button' });
                await sleep(300, 500);

                // 点击上传文件选项
                const uploadItem = page.locator('div[role="menuitem"]').filter({ hasText: /上传文件或图片|上傳檔案或圖片|Upload File or Image/ });
                await uploadFilesViaChooser(page, uploadItem, imgPaths, {
                    uploadValidator: (response) => {
                        if (response.status() !== 200 || response.request().method() !== 'POST') return false;
                        const url = response.url();
                        for (const path of expectedUploadPaths) {
                            if (url.includes(path)) return true;
                        }
                        return false;
                    }
                }, meta);
            } catch (uploadErr) {
                logger.error('适配器', `图片上传失败: ${uploadErr.message}`, meta);
                // 不抛出异常，继续尝试发送纯文本
            } finally {
                page.off('response', applyUploadHandler);
            }

            logger.info('适配器', '图片上传完成', meta);
        }

        // 4. 填写提示词
        await safeClick(page, inputLocator, { bias: 'input' });
        await humanType(page, inputLocator, prompt);

        // 5. 设置 SSE 监听
        logger.debug('适配器', '启动 SSE 监听...', meta);

        let resultText = '';
        let reasoningText = '';
        let isResolved = false;

        const resultPromise = new Promise((resolve, reject) => {
            const timeoutId = setTimeout(() => {
                if (!isResolved) {
                    isResolved = true;
                    reject(new Error(`API_TIMEOUT: 响应超时 (${Math.round(waitTimeout / 1000)}秒)`));
                }
            }, waitTimeout);

            // 监听页面响应
            const handleResponse = async (response) => {
                try {
                    const url = response.url();
                    // 只处理 chat/completion 接口的 SSE 响应
                    if (!url.includes('chat/completion')) return;

                    const contentType = response.headers()['content-type'] || '';
                    if (!contentType.includes('text/event-stream')) return;

                    // 读取响应体并解析 SSE
                    const body = await response.text();
                    const result = parseSSEResponse(body, useThinking);

                    if (result.text) {
                        resultText = result.text;
                        reasoningText = result.reasoning || '';

                        if (!isResolved) {
                            isResolved = true;
                            clearTimeout(timeoutId);
                            page.off('response', handleResponse);
                            resolve();
                        }
                    }
                } catch (e) {
                    // 忽略解析错误，继续等待
                }
            };

            page.on('response', handleResponse);
        });

        // 6. 点击发送
        const sendBtn = page.locator('button#flow-end-msg-send');
        await sendBtn.waitFor({ state: 'visible', timeout: 10000 });
        logger.info('适配器', '点击发送...', meta);
        await safeClick(page, sendBtn, { bias: 'button' });

        // 7. 等待响应
        logger.info('适配器', '等待生成结果...', meta);
        await resultPromise;

        if (resultText) {
            logger.info('适配器', `生成完成，文本长度: ${resultText.length}`, meta);
            const result = { text: resultText };
            if (reasoningText) {
                result.reasoning = reasoningText;
            }
            return result;
        } else {
            return { error: '未能从响应中提取文本' };
        }

    } catch (err) {
        const pageError = normalizePageError(err, meta);
        if (pageError) return pageError;

        logger.error('适配器', '生成任务失败', { ...meta, error: err.message });
        return { error: `生成任务失败: ${err.message}` };
    } finally { }
}

/**
 * 解析 SSE 响应体，提取最终文本
 * @param {string} body - SSE 响应体
 * @param {boolean} useThinking - 是否使用深度思考模式
 * @returns {{text: string, reasoning?: string}}
 */
function parseSSEResponse(body, useThinking) {
    const lines = body.split('\n');
    let resultText = '';
    let reasoningText = '';
    let inThinkingBlock = false;
    let thinkingBlockId = null;

    for (let i = 0; i < lines.length; i++) {
        const line = lines[i].trim();

        // 解析事件类型
        if (line.startsWith('event:')) {
            const eventType = line.substring(6).trim();

            // 找到对应的 data 行
            if (i + 1 < lines.length && lines[i + 1].startsWith('data:')) {
                const dataLine = lines[i + 1].substring(5).trim();
                if (!dataLine || dataLine === '{}') continue;

                try {
                    const data = JSON.parse(dataLine);

                    // SSE_REPLY_END with end_type: 1 的 brief 仅作兜底
                    if (eventType === 'SSE_REPLY_END' && data.end_type === 1) {
                        const brief = data.msg_finish_attr?.brief || '';
                        if (!resultText && brief) {
                            resultText = brief;
                        }
                    }

                    // STREAM_MSG_NOTIFY 检测深度思考块
                    if (eventType === 'STREAM_MSG_NOTIFY') {
                        const blocks = data.content?.content_block || [];
                        for (const block of blocks) {
                            if (block.block_type === 10040 && block.content?.thinking_block) {
                                inThinkingBlock = true;
                                thinkingBlockId = block.block_id;
                            }
                        }
                    }

                    // STREAM_CHUNK 处理内容块
                    if (eventType === 'STREAM_CHUNK' && data.patch_op) {
                        for (const op of data.patch_op) {
                            if (op.patch_object === 1 && op.patch_value?.content_block) {
                                for (const block of op.patch_value.content_block) {
                                    // 思考块结束标记
                                    if (block.block_type === 10040 && block.is_finish) {
                                        inThinkingBlock = false;
                                    }
                                    // 思考内容 (parent_id 指向 thinking_block)
                                    if (useThinking && block.parent_id === thinkingBlockId) {
                                        const text = block.content?.text_block?.text || '';
                                        if (text) reasoningText += text;
                                    }
                                    // 正文内容 (block_type 10000，非思考子块)
                                    else if (block.block_type === 10000 && block.parent_id !== thinkingBlockId) {
                                        const text = block.content?.text_block?.text || '';
                                        if (text) resultText += text;
                                    }
                                }
                            }
                        }
                    }

                    // CHUNK_DELTA 增量文本
                    if (eventType === 'CHUNK_DELTA') {
                        const text = data.text || '';
                        if (text) {
                            if (useThinking && inThinkingBlock) {
                                reasoningText += text;
                            } else {
                                resultText += text;
                            }
                        }
                    }

                } catch (e) {
                    // JSON 解析失败，跳过
                }
            }
        }
    }

    return { text: resultText, reasoning: reasoningText };
}

function parseJsonObject(value) {
    if (typeof value !== 'string') return null;
    const trimmed = value.trim();
    if (!/^[{[]/.test(trimmed)) return null;
    try {
        const parsed = JSON.parse(trimmed);
        return parsed && typeof parsed === 'object' ? parsed : null;
    } catch {
        return null;
    }
}

function transcriptText(value) {
    if (typeof value !== 'string') return '';
    const text = value.trim();
    return text && !/^(?:event|data|id|retry):/mi.test(text) ? text : '';
}

function findTranscriptInObject(value, seen = new Set()) {
    if (!value || typeof value !== 'object' || seen.has(value)) return '';
    seen.add(value);

    for (const key of ['text', 'transcript', 'transcription', 'content', 'result']) {
        const candidate = value[key];
        if (typeof candidate === 'string') {
            const nested = parseJsonObject(candidate);
            const text = nested ? findTranscriptInObject(nested, seen) : transcriptText(candidate);
            if (text) return text;
        } else {
            const text = findTranscriptInObject(candidate, seen);
            if (text) return text;
        }
    }

    for (const nested of Object.values(value)) {
        if (nested && typeof nested === 'object') {
            const text = findTranscriptInObject(nested, seen);
            if (text) return text;
        }
    }
    return '';
}

export function parseDoubaoTranscriptionResponse(body) {
    if (body && typeof body === 'object') return { text: findTranscriptInObject(body) };

    const source = String(body || '').trim();
    const directJson = parseJsonObject(source);
    if (directJson) return { text: findTranscriptInObject(directJson) };

    const isSse = /(^|\n)\s*(?:event|data|id|retry):/m.test(source);
    if (!isSse) return { text: transcriptText(source) };

    for (const line of source.split(/\r?\n/)) {
        if (!/^data:\s*/i.test(line)) continue;
        const candidate = line.replace(/^data:\s*/, '').trim();
        if (!candidate || candidate === '[DONE]') continue;
        try {
            const text = findTranscriptInObject(JSON.parse(candidate));
            if (text) return { text };
        } catch {
            // Continue parsing the remaining SSE events.
        }
    }

    const parsed = parseSSEResponse(source, false);
    if (parsed.text) return { text: parsed.text };
    return { text: '' };
}

export async function transcribe(context, payload, modelId, meta = {}) {
    if (modelId !== TRANSCRIPTION_MODEL_ID) {
        return { error: `暂不支持的豆包转写模型: ${modelId}` };
    }
    if (!payload?.filePath) return { error: '录音转写需要上传音频文件' };

    const { page, config } = context;
    const waitTimeout = config?.backend?.pool?.waitTimeout ?? 120_000;
    try {
        await gotoWithCheck(page, TARGET_URL);
        await waitForInput(page, page.locator('textarea.semi-input-textarea'), { click: false });

        await clickFirstVisible(page, [
            page.getByRole('button', { name: /更多|More/i }),
            page.locator('button').filter({ hasText: /更多/i })
        ], meta, '豆包更多菜单');
        await clickFirstVisible(page, [
            page.getByRole('menuitem', { name: /录音转写/i }),
            page.getByText(/录音转写/i)
        ], meta, '录音转写入口');
        await sleep(500, 900);

        const inputs = page.locator('input[type="file"]');
        if (await inputs.count() > 0) {
            await inputs.first().setInputFiles(payload.filePath);
        } else {
            const uploadTrigger = await firstVisibleLocator([
                page.getByRole('button', { name: /上传.*(?:录音|音频|文件)|选择.*(?:录音|音频|文件)/i }),
                page.locator('[role="button"]').filter({ hasText: /上传.*(?:录音|音频|文件)|选择.*(?:录音|音频|文件)/i })
            ], '录音文件上传');
            await uploadFilesViaChooser(page, uploadTrigger, [payload.filePath], {}, meta);
        }

        let settled = false;
        const transcriptPromise = new Promise((resolve, reject) => {
            const timeout = setTimeout(() => {
                if (!settled) {
                    settled = true;
                    page.off('response', onResponse);
                    reject(new Error(`录音转写超时 (${Math.round(waitTimeout / 1_000)} 秒)`));
                }
            }, waitTimeout);
            const onResponse = async response => {
                try {
                    const url = response.url();
                    if (!/(transcri|audio|chat\/completion)/i.test(url)) return;
                    const text = parseDoubaoTranscriptionResponse(await response.text()).text;
                    if (!text || settled) return;
                    settled = true;
                    clearTimeout(timeout);
                    page.off('response', onResponse);
                    resolve({ text });
                } catch {
                    // Non-text provider responses are not a transcription result.
                }
            };
            page.on('response', onResponse);
        });

        await clickFirstVisible(page, [
            page.getByRole('button', { name: /开始转写|转写/i }),
            page.locator('button').filter({ hasText: /开始转写|转写/i })
        ], meta, '开始转写');
        return await transcriptPromise;
    } catch (error) {
        const pageError = normalizePageError(error, meta);
        if (pageError) return pageError;
        logger.error('适配器', '豆包录音转写失败', { ...meta, error: error.message, modelId });
        const safeMessage = String(error.message || '未知错误').replaceAll(String(payload.filePath), '已上传音频');
        return { error: `豆包录音转写失败: ${safeMessage}` };
    }
}

/**
 * 适配器 manifest
 */
export const manifest = {
    id: 'doubao_text',
    displayName: '豆包 (文本生成)',
    description: '使用字节跳动豆包生成文本，支持专家、工作任务 Turbo、深入研究和录音转写。需要已登录的豆包账户。',

    getTargetUrl(config, workerConfig) {
        return TARGET_URL;
    },

    models: [
        { id: 'seed', imagePolicy: 'optional', type: 'text' },
        { id: 'seed-thinking', imagePolicy: 'optional', type: 'text' },
        { id: 'seed-pro', imagePolicy: 'optional', type: 'text', capabilities: ['expert_mode'] },
        { id: 'doubao-work-task-turbo', imagePolicy: 'optional', type: 'text', capabilities: ['work_task_turbo'] },
        { id: 'doubao-deep-research', imagePolicy: 'optional', type: 'text', capabilities: ['deep_research'] },
        { id: 'doubao-transcription', imagePolicy: 'forbidden', type: 'transcription', capabilities: ['audio_transcription'] }
    ],

    navigationHandlers: [],

    generate,
    transcribe
};
