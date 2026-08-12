/**
 * Safe, user-facing model metadata for the Doubao Web integration.
 *
 * Values marked as `source: 'web'` are read from the authenticated Doubao
 * controls. They describe the visible product surface only; no private
 * account/session material is included here.
 */

export const DOUBAO_PROVIDER = Object.freeze({
    id: 'doubao',
    displayName: '豆包'
});

const imageStyleValues = [
    '人像摄影', '电影写真', '中国风', '动漫', '3D渲染', '赛博朋克',
    'CG 动画', '水墨画', '油画', '古典', '水彩画', '卡通', '平面插画',
    '风景', '港风动漫', '像素风格', '荧光绘画', '彩铅画', '手办',
    '儿童绘画', '抽象', '锐笔插画', '二次元', '油墨印刷', '版画',
    '莫奈', '毕加索', '伦勃朗', '马蒂斯', '巴洛克', '复古动漫', '绘本'
];

export const DOUBAO_IMAGE_PARAMETERS = Object.freeze([
    {
        key: 'ratio',
        label: '比例',
        values: ['自动', '9:16', '2:3', '3:4', '1:1', '4:3', '3:2', '16:9'],
        source: 'web'
    },
    {
        key: 'style',
        label: '风格',
        values: imageStyleValues,
        source: 'web'
    },
    {
        key: 'reference_images',
        label: '参考图',
        values: ['最多 10 张'],
        source: 'adapter'
    },
    {
        key: 'resolution',
        label: '清晰度',
        values: [],
        note: '豆包网页当前未显示 480p、720p 等固定清晰度选项。',
        source: 'web'
    }
]);

export const DOUBAO_VIDEO_PARAMETERS = Object.freeze([
    {
        key: 'ratio',
        label: '比例',
        values: ['自动', '3:4', '4:3', '9:16', '16:9', '1:1', '21:9'],
        source: 'web'
    },
    {
        key: 'duration',
        label: '时长',
        values: ['4s', '10s', '15s'],
        source: 'web'
    },
    {
        key: 'reference_images',
        label: '参考图',
        values: ['支持图生视频'],
        source: 'adapter'
    },
    {
        key: 'resolution',
        label: '清晰度',
        values: [],
        note: '豆包网页当前未显示 480p、720p 等固定清晰度选项。',
        source: 'web'
    }
]);

export const DOUBAO_TEXT_PARAMETERS = Object.freeze([
    { key: 'stream', label: '流式响应', values: ['开启', '关闭'], source: 'api' },
    { key: 'reasoning', label: '思考返回', values: ['开启', '关闭'], source: 'api' }
]);

export const DOUBAO_TRANSCRIPTION_PARAMETERS = Object.freeze([
    { key: 'audio_file', label: '音频文件', values: ['必填'], source: 'api' },
    { key: 'response_format', label: '返回格式', values: ['json', 'text', 'verbose_json'], source: 'api' }
]);

export const DOUBAO_MUSIC_PARAMETERS = Object.freeze([
    { key: 'lyric', label: '歌词', values: ['可选'], source: 'adapter' },
    { key: 'genre', label: '曲风', values: ['可选'], source: 'adapter' },
    { key: 'mood', label: '情绪', values: ['可选'], source: 'adapter' },
    { key: 'voice', label: '人声', values: ['可选'], source: 'adapter' }
]);
