/**
 * @fileoverview Claude.ai 文本生成适配器
 */

import {
    sleep,
    humanType,
    safeClick
} from '../engine/utils.js';
import {
    normalizePageError,
    waitForInput,
    gotoWithCheck
} from '../utils/index.js';
import { logger } from '../../utils/logger.js';

const TARGET_URL = 'https://claude.ai/new';
const INPUT_SELECTOR = '[contenteditable="true"], textarea';

async function selectModel(page, modelConfig, meta = {}) {
    const codeName = modelConfig?.codeName;
    if (!codeName) return false;

    try {
        const candidates = [
            page.locator('[data-testid*="model" i]').filter({ hasText: /Claude|Sonnet|Haiku|Opus/i }),
            page.getByRole('button', { name: /Claude|Sonnet|Haiku|Opus|model/i }),
            page.locator('button').filter({ hasText: /Claude|Sonnet|Haiku|Opus/i })
        ];

        let modelSelectorBtn = null;
        for (const candidate of candidates) {
            if (await candidate.count().catch(() => 0)) {
                modelSelectorBtn = candidate.first();
                break;
            }
        }

        if (!modelSelectorBtn) {
            logger.debug('适配器', '未找到 Claude 模型选择器，使用当前默认模型', meta);
            return false;
        }

        await safeClick(page, modelSelectorBtn, { bias: 'button', timeout: 5000 });
        await sleep(500, 800);

        const targetItem = page.getByRole('menuitem', { name: new RegExp(codeName, 'i') })
            .or(page.getByRole('option', { name: new RegExp(codeName, 'i') }))
            .or(page.locator('[role="menuitem"], [role="option"], button').filter({ hasText: new RegExp(codeName, 'i') }))
            .first();

        if (await targetItem.count().catch(() => 0)) {
            logger.info('适配器', `正在选择模型: ${codeName}`, meta);
            await safeClick(page, targetItem, { bias: 'button', timeout: 5000 });
            await sleep(300, 500);
            return true;
        }

        logger.debug('适配器', `未找到 Claude 模型 ${codeName}，使用当前默认模型`, meta);
        await page.keyboard.press('Escape').catch(() => { });
        return false;
    } catch (e) {
        logger.warn('适配器', `选择 Claude 模型失败: ${e.message}，使用当前默认模型`, meta);
        await page.keyboard.press('Escape').catch(() => { });
        return false;
    }
}

async function clickSend(page, inputLocator, meta = {}) {
    const candidates = [
        page.locator('[data-testid*="send" i]').last(),
        page.getByRole('button', { name: /send|发送/i }).last(),
        page.locator('button[aria-label*="Send"], button[aria-label*="send"], button[aria-label*="发送"]').last()
    ];

    for (const locator of candidates) {
        try {
            await locator.waitFor({ state: 'visible', timeout: 5000 });
            if (!(await locator.isEnabled().catch(() => false))) continue;
            await locator.click({ force: true, timeout: 5000 });
            return;
        } catch (e) {
            logger.debug('适配器', `Claude 发送按钮候选失败: ${e.message}`, meta);
        }
    }

    logger.warn('适配器', 'Claude 发送按钮点击失败，尝试键盘 Enter 发送', meta);
    await safeClick(page, inputLocator, { bias: 'input', timeout: 5000 });
    await page.keyboard.press('Enter');
}

function extractClaudeAssistantText(promptText = '') {
    const rejectExact = new Set(['Claude', 'Thinking', '思考中']);
    const clean = (value) => (value || '')
        .replace(/^Claude said:\s*/i, '')
        .replace(/ /g, ' ')
        .trim();
    const acceptable = (value) => {
        const text = clean(value);
        if (!text || rejectExact.has(text)) return '';
        if (promptText && text === clean(promptText)) return '';
        if (/^(Claude|Thinking|思考中)\s*$/i.test(text)) return '';
        return text;
    };

    const selectors = [
        '[data-testid*="assistant" i]',
        '[data-message-author-role="assistant"]',
        '[class*="assistant" i]',
        '[class*="message" i]'
    ];

    const seen = new Set();
    const candidates = [];
    for (const selector of selectors) {
        for (const node of document.querySelectorAll(selector)) {
            if (!seen.has(node)) {
                seen.add(node);
                candidates.push(node);
            }
        }
    }

    for (let i = candidates.length - 1; i >= 0; i--) {
        const node = candidates[i];
        const preferred = Array.from(node.querySelectorAll('.prose, .markdown, [data-testid*="content" i]'));
        for (let j = preferred.length - 1; j >= 0; j--) {
            const text = acceptable(preferred[j].innerText || preferred[j].textContent);
            if (text) return text;
        }

        const text = acceptable(node.innerText || node.textContent);
        if (text) return text;
    }

    return '';
}

function isClaudeGenerating() {
    const text = document.body.innerText || '';
    if (/Thinking\.\.\.|Thinking…|思考中|Claude is responding|正在回复/.test(text)) return true;
    const buttons = Array.from(document.querySelectorAll('button'));
    return buttons.some((button) => {
        const label = `${button.getAttribute('aria-label') || ''} ${button.innerText || button.textContent || ''}`;
        return /stop|停止|cancel|取消/i.test(label);
    });
}


function normalizeClaudeFinalText(value) {
    let text = (value || '').trim();
    text = text.replace(/^Claude responded:\s*/i, '').trim();
    const lines = text.split('\n')
        .map(line => line.trim())
        .filter(line => line && !/^Thought for\s+\d+s$/i.test(line));
    if (lines.length >= 2 && lines[0] === lines[1]) lines.shift();
    return lines.join('\n').trim();
}

async function waitForFinalText(page, waitTimeout, prompt, meta = {}) {
    const domWaitTimeout = Math.min(waitTimeout, 180000);
    await page.waitForFunction(extractClaudeAssistantText, prompt, { timeout: domWaitTimeout }).catch(() => { });

    let finalText = '';
    let lastText = '';
    let stableCount = 0;
    const startedAt = Date.now();

    while (Date.now() - startedAt < domWaitTimeout) {
        const currentText = await page.evaluate(extractClaudeAssistantText, prompt).catch(() => '');
        const generating = await page.evaluate(isClaudeGenerating).catch(() => false);

        if (currentText && currentText === lastText && !generating) {
            stableCount++;
        } else {
            stableCount = 0;
            lastText = currentText || lastText || '';
        }

        if (lastText && !generating && stableCount >= 5) {
            finalText = lastText;
            break;
        }

        await sleep(1200, 1600);
    }

    if (!finalText) finalText = lastText || await page.evaluate(extractClaudeAssistantText, prompt).catch(() => '');
    finalText = normalizeClaudeFinalText(finalText);
    if (finalText) logger.info('适配器', `Claude DOM 提取文本成功 (${finalText.length} 字符)`, meta);
    return finalText;
}

async function generate(context, prompt, imgPaths, modelId, meta = {}) {
    const { page, config } = context;
    const waitTimeout = config?.backend?.pool?.waitTimeout ?? 120000;
    const inputLocator = page.locator(INPUT_SELECTOR).last();

    try {
        if (imgPaths && imgPaths.length > 0) {
            return { error: 'Claude 网页适配器暂不支持图片输入' };
        }

        logger.info('适配器', '开启 Claude 新会话...', meta);
        await gotoWithCheck(page, TARGET_URL);

        await waitForInput(page, inputLocator, { click: false, timeout: 90000 });

        const modelConfig = manifest.models.find(m => m.id === modelId);
        if (modelConfig) await selectModel(page, modelConfig, meta);

        logger.info('适配器', '输入提示词...', meta);
        await inputLocator.click({ force: true, timeout: 10000 });
        await humanType(page, inputLocator, prompt, { skipFocus: true });
        await sleep(300, 500);

        logger.info('适配器', '发送提示词...', meta);
        await clickSend(page, inputLocator, meta);

        logger.info('适配器', '等待 Claude 生成结果...', meta);
        const text = await waitForFinalText(page, waitTimeout, prompt, meta);

        if (!text || !text.trim()) {
            logger.warn('适配器', 'Claude 回复内容为空', meta);
            return { error: 'Claude 回复内容为空；请确认已登录且账号仍有可用额度' };
        }

        logger.info('适配器', `已获取 Claude 文本内容 (${text.length} 字符)`, meta);
        return { text: text.trim() };
    } catch (err) {
        const pageError = normalizePageError(err, meta);
        if (pageError) return pageError;

        logger.error('适配器', 'Claude 生成任务失败', { ...meta, error: err.message });
        return { error: `Claude 生成任务失败: ${err.message}` };
    }
}

export const manifest = {
    id: 'claude_text',
    displayName: 'Claude.ai (文本生成)',
    description: '使用 Claude.ai 官网生成文本。需要在浏览器中登录 Claude 账号。',

    configSchema: [],

    getTargetUrl() {
        return TARGET_URL;
    },

    models: [
        { id: 'claude-sonnet-5', codeName: 'Sonnet', imagePolicy: 'forbidden', type: 'text' },
        { id: 'claude-auto', codeName: '', imagePolicy: 'forbidden', type: 'text' }
    ],

    navigationHandlers: [],
    generate
};
