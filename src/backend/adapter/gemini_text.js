/**
 * @fileoverview Google Gemini 文本生成适配器
 */

import {
    sleep,
    humanType,
    safeClick,
    uploadFilesViaChooser
} from '../engine/utils.js';
import {
    normalizePageError,
    normalizeHttpError,
    waitForInput,
    gotoWithCheck,
    waitApiResponse
} from '../utils/index.js';
import { logger } from '../../utils/logger.js';

// --- 配置常量 ---
const TARGET_URL = 'https://gemini.google.com/app?hl=en';

async function clickGeminiSend(page, inputLocator, sendBtnLocator, meta = {}) {
    const waitForEnabled = async (locator, timeout = 5000) => {
        await locator.waitFor({ state: 'visible', timeout });
        const started = Date.now();
        while (Date.now() - started < timeout) {
            if (await locator.isEnabled().catch(() => false)) return;
            await sleep(100, 150);
        }
        throw new Error('send button disabled');
    };

    const candidates = [
        sendBtnLocator.last(),
        page.getByRole('button', { name: /send message|send|submit/i }).last(),
        page.locator('button[aria-label*="Send"], button[aria-label*="Submit"]').last(),
        page.locator('button:has(mat-icon)').filter({ hasText: /send/i }).last()
    ];

    for (const locator of candidates) {
        try {
            await waitForEnabled(locator);
            await locator.click({ force: true, timeout: 5000 });
            return;
        } catch (e) {
            logger.debug('适配器', `发送按钮点击候选失败: ${e.message}`, meta);
        }
    }

    logger.warn('适配器', '发送按钮点击失败，尝试键盘 Enter 发送', meta);
    await safeClick(page, inputLocator, { bias: 'input', timeout: 5000 });
    await page.keyboard.press('Enter');
}

/**
 * 执行文本生成任务
 * @param {object} context - 浏览器上下文 { page, config }
 * @param {string} prompt - 提示词
 * @param {string[]} imgPaths - 图片路径数组
 * @param {string} [modelId] - 模型 ID (此适配器未使用)
 * @param {object} [meta={}] - 日志元数据
 * @returns {Promise<{text?: string, reasoning?: string, error?: string}>}
 */
async function generate(context, prompt, imgPaths, modelId, meta = {}) {
    const { page, config } = context;
    const waitTimeout = config?.backend?.pool?.waitTimeout ?? 120000;
    const inputLocator = page.getByRole('textbox');
    const sendBtnLocator = page.getByRole('button', { name: 'Send message' });

    try {
        logger.info('适配器', '开启新会话...', meta);
        await gotoWithCheck(page, TARGET_URL);

        const useTempChat = config?.backend?.adapter?.gemini_text?.temporaryChat || false;
        if (useTempChat) {
            try {
                logger.debug('适配器', '尝试点击 Temporary chat...', meta);
                const tempChatBtn = page.getByRole('button', { name: 'Temporary chat' });
                await safeClick(page, tempChatBtn, { bias: 'button', timeout: 3000 });
            } catch (e) {
                logger.debug('适配器', '未找到 Temporary chat 按钮或点击失败，忽略', meta);
            }
        }

        // 1. 等待输入框加载
        await waitForInput(page, inputLocator, { click: false });

        // 2. 上传图片
        if (imgPaths && imgPaths.length > 0) {
            logger.info('适配器', `开始上传 ${imgPaths.length} 张图片...`, meta);
            logger.debug('适配器', '点击加号按钮...', meta);
            const uploadMenuBtn = page.getByRole('button', { name: 'Open upload file menu' });
            await safeClick(page, uploadMenuBtn, { bias: 'button' });

            const uploadFilesBtn = page.getByRole('menuitem', { name: /Upload files/ });
            await uploadFilesViaChooser(page, uploadFilesBtn, imgPaths, {
                uploadValidator: (response) => {
                    const url = response.url();
                    return response.status() === 200 &&
                        url.includes('google.com/upload/') &&
                        url.includes('upload_id=');
                }
            }, meta);
            logger.info('适配器', '图片上传完成', meta);
        }

        // 3. 输入提示词
        logger.info('适配器', '输入提示词...', meta);
        await safeClick(page, inputLocator, { bias: 'input' });
        await humanType(page, inputLocator, prompt);

        // 4. 选择模型
        if (modelId) {
            try {
                logger.debug('适配器', `准备选择模型: ${modelId}`, meta);

                // 点击打开模型选择菜单
                const modePickerBtn = page.getByRole('button', { name: 'Open mode picker' });
                await safeClick(page, modePickerBtn, { bias: 'button' });
                await sleep(300, 500);

                // 获取所有 menuitem 选项的文本
                const menuItemsLocator = page.getByRole('menuitem');
                const menuItemsCount = await menuItemsLocator.count();

                if (menuItemsCount === 0) {
                    logger.warn('适配器', '未找到模型选项，使用默认模型', meta);
                } else {
                    // 获取所有选项的文本（去除前后空白）
                    const itemTexts = await menuItemsLocator.allTextContents();

                    logger.debug('适配器', `可用模型选项: [${itemTexts.map(t => t.trim()).join('], [')}]`, meta);

                    // 判断是否有 Pro 选项
                    const hasPro = itemTexts.some(text => text.trim().startsWith('Pro'));

                    // 确定要选择的目标选项文本前缀
                    let targetPrefix = null;

                    if (hasPro) {
                        // 有 Pro 选项的情况
                        if (modelId === 'gemini-3.1-pro') {
                            targetPrefix = 'Pro';
                        } else if (modelId === 'gemini-3.1-flash-thinking') {
                            targetPrefix = 'Thinking';
                        } else {
                            targetPrefix = 'Fast';
                        }
                    } else {
                        // 没有 Pro 选项的情况
                        if (modelId === 'gemini-3.1-pro' || modelId === 'gemini-3.1-flash-thinking') {
                            targetPrefix = 'Thinking';
                        } else {
                            targetPrefix = 'Fast';
                        }
                    }

                    logger.debug('适配器', `目标模型前缀: "${targetPrefix}"`, meta);

                    // 使用 locator 直接定位目标选项（避免缓存元素引用导致 detached 错误）
                    const targetItem = menuItemsLocator.filter({ hasText: new RegExp(`^\\s*${targetPrefix}`) }).first();

                    if (await targetItem.count() > 0) {
                        const selectedText = (await targetItem.textContent() || '').trim();
                        await safeClick(page, targetItem, { bias: 'button' });
                        logger.info('适配器', `已选择模型: "${selectedText}"`, meta);
                    } else {
                        logger.warn('适配器', `未找到匹配的模型选项 (${targetPrefix})，使用默认模型`, meta);
                        // 按 Escape 关闭菜单
                        await page.keyboard.press('Escape');
                    }
                }
            } catch (e) {
                logger.warn('适配器', `模型选择失败: ${e.message}，继续使用默认模型`, meta);
                // 尝试关闭可能打开的菜单
                try {
                    await page.keyboard.press('Escape');
                } catch { }
            }
        }

        // 5. 先启动 API 监听
        logger.debug('适配器', '启动 API 监听...', meta);
        const apiResponsePromise = waitApiResponse(page, {
            urlMatch: 'assistant.lamda.BardFrontendService/StreamGenerate',
            method: 'POST',
            timeout: waitTimeout,
            meta
        });

        // 6. 发送提示词
        logger.info('适配器', '发送提示词...', meta);
        await clickGeminiSend(page, inputLocator, sendBtnLocator, meta);

        logger.info('适配器', '等待生成结果...', meta);

        // 7. 等待 API 响应
        let apiResponse;
        try {
            apiResponse = await apiResponsePromise;
        } catch (e) {
            const pageError = normalizePageError(e, meta);
            if (pageError) return pageError;
            throw e;
        }

        // 检查 HTTP 错误
        const httpError = normalizeHttpError(apiResponse);
        if (httpError) {
            logger.error('适配器', `API 返回错误: ${httpError.error}`, meta);
            return { error: `API 返回错误: ${httpError.error}` };
        }

        // 6. 解析响应体
        const bodyBuffer = await apiResponse.body();
        logger.debug('适配器', `收到响应体，字节数: ${bodyBuffer.length}`, meta);

        const { text, reasoning } = getFinalAiTextFromResponse(bodyBuffer);

        if (text) {
            logger.info('适配器', `解析成功，文本长度: ${text.length}，思考长度: ${reasoning?.length || 0}`, meta);
            return reasoning ? { text, reasoning } : { text };
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
 * 适配器 manifest
 */
export const manifest = {
    id: 'gemini_text',
    displayName: 'Google Gemini (文本生成)',
    description: '使用 Google Gemini 官网生成文本，支持多模型切换和图片上传。需要已登录的 Google 账户。',

    // 配置项模式
    configSchema: [
        {
            key: 'temporaryChat',
            label: '临时对话',
            type: 'boolean',
            default: false,
            note: '开启后将使用临时对话模式'
        }
    ],

    getTargetUrl(config, workerConfig) {
        return TARGET_URL;
    },

    models: [
        { id: 'gemini-3.1-flash', imagePolicy: 'optional', type: 'text' },
        { id: 'gemini-3.1-flash-thinking', imagePolicy: 'optional', type: 'text' },
        { id: 'gemini-3.1-pro', imagePolicy: 'optional', type: 'text' }
    ],

    navigationHandlers: [],

    generate
};

// ==========================================
// 解析 gRPC Batchexecute
// ==========================================

/**
 * 解析 batchexecute/batch RPC 响应（直接操作 Buffer）
 * @param {Buffer} buf - 响应体 Buffer
 */
function parseLenFramedResponse(buf) {
    let i = 0;

    // 去掉 )]}\' 这种 XSSI 前缀（通常是第一行）
    if (buf.length >= 4 && buf[0] === 0x29 && buf[1] === 0x5d && buf[2] === 0x7d) {
        const firstNl = buf.indexOf(0x0a);
        if (firstNl !== -1) i = firstNl + 1;
    }

    const frames = [];

    const readLineBuf = () => {
        if (i >= buf.length) return null;
        const nl = buf.indexOf(0x0a, i);
        let line;
        if (nl === -1) {
            line = buf.slice(i);
            i = buf.length;
        } else {
            line = buf.slice(i, nl);
            i = nl + 1;
        }
        // strip trailing \r
        if (line.length && line[line.length - 1] === 0x0d) line = line.slice(0, -1);
        return line;
    };

    let pendingLen = null;

    while (true) {
        const lineBuf = readLineBuf();
        if (lineBuf === null) break;

        const lineStr = lineBuf.toString('utf8').trim();
        if (!lineStr) continue;

        // 先找长度行（纯数字）
        if (pendingLen === null) {
            if (/^\d+$/.test(lineStr)) pendingLen = Number(lineStr);
            continue;
        }

        // 读到 payload 行；大多数情况下 payload 是单行 JSON。
        // 这里**不依赖** pendingLen 的数值（它有时会不准），而是：
        //   1) 先尝试解析当前行
        //   2) 若报“JSON 未结束”一类错误，再把后续行拼上重试（极少见）
        let chunkBuf = lineBuf;
        let chunkStr = chunkBuf.toString('utf8').trim();

        while (true) {
            try {
                frames.push(JSON.parse(chunkStr));
                break;
            } catch (e) {
                // 只有在明显是“截断/未结束”的情况下，才继续拼下一行
                const msg = String(e && e.message || '');
                const looksTruncated = /Unexpected end of JSON input|Unterminated string/.test(msg);

                if (!looksTruncated) {
                    const head = chunkStr.slice(0, 220);
                    const tail = chunkStr.slice(-220);
                    throw new Error(
                        `Chunk JSON parse failed: ${msg}\n` +
                        `Chunk head: ${head}\n` +
                        `Chunk tail: ${tail}\n` +
                        `LenHeader: ${pendingLen} | ActualBytes: ${Buffer.byteLength(chunkStr, 'utf8')}`
                    );
                }

                // 读取下一行进行拼接，但如果下一行是“纯数字长度行”，不要吞掉它
                const savedPos = i;
                const next = readLineBuf();
                if (next === null) {
                    const head = chunkStr.slice(0, 220);
                    const tail = chunkStr.slice(-220);
                    throw new Error(
                        `Chunk JSON parse failed: ${msg} (EOF)\n` +
                        `Chunk head: ${head}\n` +
                        `Chunk tail: ${tail}\n` +
                        `LenHeader: ${pendingLen} | ActualBytes: ${Buffer.byteLength(chunkStr, 'utf8')}`
                    );
                }

                const nextStr = next.toString('utf8').trim();
                if (/^\d+$/.test(nextStr)) {
                    // 回退，交给外层当作下一段的 length line
                    i = savedPos;
                    const head = chunkStr.slice(0, 220);
                    const tail = chunkStr.slice(-220);
                    throw new Error(
                        `Chunk JSON parse failed: ${msg} (hit next length line)\n` +
                        `Chunk head: ${head}\n` +
                        `Chunk tail: ${tail}\n` +
                        `LenHeader: ${pendingLen} | ActualBytes: ${Buffer.byteLength(chunkStr, 'utf8')}`
                    );
                }

                // 把分隔符 \n 加回去
                chunkBuf = Buffer.concat([chunkBuf, Buffer.from('\n'), next]);
                chunkStr = chunkBuf.toString('utf8').trim();
            }
        }

        pendingLen = null;
    }

    return frames;
}

/**
 * 把 frame 里的 payload 再 parse 一次
 */
function extractPayloads(frames) {
    const payloads = [];
    for (const frame of frames) {
        if (!Array.isArray(frame)) continue;

        // frame 可能是 [["wrb.fr", null, "<jsonstr>"]] 也可能有多个 item
        for (const item of frame) {
            if (!Array.isArray(item)) continue;
            const payloadStr = item[2];
            if (typeof payloadStr !== "string") continue;

            try {
                payloads.push(JSON.parse(payloadStr));
            } catch {
                // ignore non-payload frames
            }
        }
    }
    return payloads;
}

/**
 * 在任意嵌套结构里，找形如 ["rc_xxx", ["text..."], ...] 的节点
 */
function collectRcTextsDeep(root) {
    const bestByRc = new Map();

    const stack = [root];
    while (stack.length) {
        const cur = stack.pop();
        if (!cur) continue;

        if (Array.isArray(cur)) {
            const maybeRc = cur[0];
            const maybeArr = cur[1];
            if (
                typeof maybeRc === "string" &&
                maybeRc.startsWith("rc_") &&
                Array.isArray(maybeArr)
            ) {
                const text = maybeArr.filter(v => typeof v === "string").join("");
                if (text) {
                    const prev = bestByRc.get(maybeRc) || "";
                    if (text.length >= prev.length) bestByRc.set(maybeRc, text);
                }
            }

            for (const v of cur) stack.push(v);
        } else if (typeof cur === "object") {
            for (const v of Object.values(cur)) stack.push(v);
        }
    }

    return bestByRc;
}

/**
 * 从单个 payload 中提取文本和 thinking 内容
 * @param {Array} payload - 解析后的 payload
 * @returns {{text: string, thinking: string}}
 */
function extractTextAndThinking(payload) {
    let text = '';
    let thinking = '';

    try {
        if (!Array.isArray(payload)) return { text, thinking };

        // 找 rc 节点 (通常在 payload[4][0])
        // 结构: payload[4][0] = ["rc_xxx", ["text..."], ..., [37]: [[thinking]]]
        let rc = null;
        if (payload[4] && Array.isArray(payload[4][0]) &&
            typeof payload[4][0][0] === 'string' && payload[4][0][0].startsWith('rc_')) {
            rc = payload[4][0];
        }

        if (!rc) return { text, thinking };

        // 文本在 rc[1][0]
        if (Array.isArray(rc[1]) && typeof rc[1][0] === 'string') {
            text = rc[1][0];
        }

        // thinking 在 rc[37][0][0]
        // 结构: rc[37] = [["**Thinking Title**\n\nThinking content..."]]
        try {
            if (rc[37] && Array.isArray(rc[37]) && rc[37][0] && Array.isArray(rc[37][0])) {
                if (typeof rc[37][0][0] === 'string') {
                    thinking = rc[37][0][0];
                }
            }
        } catch {
            // thinking 提取失败，忽略
        }
    } catch {
        // ignore
    }

    return { text, thinking };
}

/**
 * 从响应体 Buffer 中提取最终 AI 文本和 thinking
 * @param {Buffer} bodyBuffer - 响应体 Buffer
 * @returns {{text: string, reasoning: string}}
 */
function getFinalAiTextFromResponse(bodyBuffer) {
    const frames = parseLenFramedResponse(bodyBuffer);
    const payloads = extractPayloads(frames);

    let bestText = '';
    let bestThinking = '';

    // 遍历所有 payload，保留最长的 text 和对应的 thinking
    for (const payload of payloads) {
        const { text, thinking } = extractTextAndThinking(payload);
        if (text.length > bestText.length) {
            bestText = text;
            bestThinking = thinking;
        }
    }

    // 如果 extractTextAndThinking 没找到（可能结构不匹配），fallback 到 collectRcTextsDeep
    if (!bestText) {
        for (const payload of payloads) {
            const m = collectRcTextsDeep(payload);
            for (const text of m.values()) {
                if (text.length > bestText.length) bestText = text;
            }
        }
    }

    return { text: bestText, reasoning: bestThinking };
}