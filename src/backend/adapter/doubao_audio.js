/**
 * Doubao Web music adapter. It shares the logged-in Doubao browser page with
 * image/video work and returns a normal media-job result instead of base64.
 */

import crypto from 'crypto';
import { gotoWithCheck } from '../utils/index.js';
import { logger } from '../../utils/logger.js';
import { asJson, browserSsePost, decodePossibleUrl, parseSse } from './doubao_video.js';

const TARGET_URL = 'https://www.doubao.com/chat/';

function buildMusicPayload(prompt, options = {}) {
    const settings = options.provider_options || {};
    const lyric = settings.lyric || '';
    const genre = settings.genre || 'Pop';
    const mood = settings.mood || 'Happy';
    const gender = settings.voice || settings.gender || 'Female';
    const generationType = lyric ? 'custome_lyric' : 'AI_lyric';
    const variables = {
        lyric,
        theme: lyric ? '' : prompt,
        mood,
        genre,
        gender,
        generation_type: generationType
    };
    const message = {
        content: JSON.stringify({ text: prompt, ...variables }),
        content_type: 2005,
        attachments: [],
        references: [],
        skill: { skill_type: 9, skill_type_no_default: 9, skill_id: '9', skill_id_no_default: '9' },
        ext: { input_skill: JSON.stringify({ skill_id: '9', skill_type: 9, variables }) }
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
            action_bar_skill_id: 9
        },
        evaluate_option: { web_ab_params: '' },
        local_conversation_id: crypto.randomUUID(),
        local_message_id: crypto.randomUUID()
    };
}

export function parseMusicResult(raw) {
    const tracks = [];
    let lastContent = null;
    for (const event of parseSse(raw)) {
        const eventData = asJson(event.event_data);
        if (event.event_type === 2005) {
            const message = typeof eventData === 'string' ? eventData : JSON.stringify(eventData || {});
            return { error: `豆包音乐请求失败: ${message.slice(0, 500)}` };
        }
        const message = asJson(eventData?.message);
        if (!message || ![2004, 2006].includes(message.content_type)) continue;
        const content = asJson(message.content);
        if (content && typeof content === 'object') lastContent = content;
    }
    if (!lastContent) return { tracks: [], message: '豆包未返回音乐内容' };

    const values = Array.isArray(lastContent.tasks)
        ? lastContent.tasks
        : Object.values(lastContent.tasks || {});
    for (const task of values) {
        if (!task || typeof task !== 'object') continue;
        if (task.music_gen_failed) {
            return { error: task.music_gen_failed_msg || '豆包音乐生成失败' };
        }
        let audioUrl = task.audio_url || task.play_url || task.url || '';
        const videoModel = asJson(task.video_model);
        if (!audioUrl && videoModel?.video_list) {
            for (const item of Object.values(videoModel.video_list)) {
                audioUrl = decodePossibleUrl(item?.main_url || item?.backup_url);
                if (audioUrl) break;
            }
        }
        if (!audioUrl) continue;
        const cover = task.cover || {};
        tracks.push({
            audio_url: audioUrl,
            title: task.title || '',
            lyrics: task.lyric || '',
            duration: task.duration || task.video_duration || videoModel?.video_duration || 0,
            cover_url: task.cover_url || cover.image_ori?.url || cover.image_thumb?.url || ''
        });
    }
    return { tracks, message: tracks.length ? '音乐已生成' : '豆包音乐仍在生成或未返回音频链接' };
}

export async function createAudio(context, payload, modelId, meta = {}) {
    if (modelId !== 'doubao-music') return { error: `暂不支持的豆包音频模型: ${modelId}` };
    try {
        await gotoWithCheck(context.page, TARGET_URL);
        const raw = await browserSsePost(context.page, '/samantha/chat/completion', buildMusicPayload(payload.prompt, payload.options), 300_000);
        const result = parseMusicResult(raw);
        if (result.error) return result;
        if (!result.tracks.length) return { error: result.message };
        return result;
    } catch (error) {
        logger.error('适配器', '豆包音乐生成失败', { ...meta, error: error.message, modelId });
        return { error: `豆包音乐生成失败: ${error.message}` };
    }
}
