/**
 * 升级点单元验证：Anthropic 转换 / OpenAPI / 错误码
 * 运行: node scripts/test-upgrade.mjs
 */
import { convertAnthropicToInternal } from '../src/server/api/anthropic/routes.js';
import { buildOpenApiSchema } from '../src/server/api/openai/openapi.js';
import { ERROR_CODES, getErrorStatus } from '../src/server/errors.js';
import { buildAnthropicMessage, buildAnthropicMessageEvents } from '../src/server/respond.js';

let passed = 0;
let failed = 0;

function assert(name, cond, detail = '') {
    if (cond) {
        passed++;
        console.log(`  ✓ ${name}`);
    } else {
        failed++;
        console.error(`  ✗ ${name}${detail ? ' — ' + detail : ''}`);
    }
}

console.log('\n[1] Anthropic → Internal 转换');
{
    const body = {
        model: 'claude-text',
        system: 'You are helpful',
        messages: [
            { role: 'user', content: 'Hello' },
            {
                role: 'assistant',
                content: [
                    { type: 'thinking', thinking: 'thinking...' },
                    { type: 'text', text: 'Hi there' }
                ]
            },
            {
                role: 'user',
                content: [
                    { type: 'text', text: 'What is in this image?' },
                    {
                        type: 'image',
                        source: { type: 'base64', media_type: 'image/png', data: 'aGVsbG8=' }
                    }
                ]
            }
        ],
        stream: true,
        max_tokens: 1024
    };
    const internal = convertAnthropicToInternal(body);
    assert('model 透传', internal.model === 'claude-text');
    assert('stream 透传', internal.stream === true);
    assert('强制 stateless', internal.stateless === true);
    assert('system 转为 system 消息', internal.messages[0]?.role === 'system' && internal.messages[0]?.content === 'You are helpful');
    assert('第一条 user 保留', internal.messages[1]?.role === 'user' && internal.messages[1]?.content === 'Hello');
    assert('assistant blocks 合并', internal.messages[2]?.role === 'assistant');
    assert('含 image content parts', Array.isArray(internal.messages[3]?.content));
    const parts = internal.messages[3].content;
    const hasImage = parts.some(p => p.type === 'image_url' && p.image_url.url.startsWith('data:image/png;base64,'));
    assert('image block 转为 image_url', hasImage);
}

console.log('\n[2] Anthropic 响应构造');
{
    const resp = buildAnthropicMessage('Hello world', 'test-model', 'I am thinking');
    assert('type=message', resp.type === 'message');
    assert('role=assistant', resp.role === 'assistant');
    assert('content 含 thinking + text', resp.content.length === 2
        && resp.content[0].type === 'thinking'
        && resp.content[1].type === 'text'
        && resp.content[1].text === 'Hello world');
    assert('stop_reason=end_turn', resp.stop_reason === 'end_turn');
    assert('usage 存在', typeof resp.usage?.output_tokens === 'number');

    const events = buildAnthropicMessageEvents('Hi', 'm', null);
    const joined = events.join('');
    assert('SSE 含 message_start', joined.includes('message_start'));
    assert('SSE 含 content_block_delta', joined.includes('text_delta'));
    assert('SSE 含 message_stop', joined.includes('message_stop'));
}

console.log('\n[3] OpenAPI schema');
{
    const schema = buildOpenApiSchema({
        models: { object: 'list', data: [{ id: 'foo', type: 'text' }, { id: 'bar', type: 'image' }] },
        version: '3.7.0'
    });
    assert('openapi 3.0.3', schema.openapi === '3.0.3');
    assert('含 /health', !!schema.paths['/health']);
    assert('含 /ready', !!schema.paths['/ready']);
    assert('含 /v1/messages', !!schema.paths['/v1/messages']);
    assert('含 /v1/stateless/chat/completions', !!schema.paths['/v1/stateless/chat/completions']);
    assert('含 /v1/providers', !!schema.paths['/v1/providers']);
    assert('含 /v1/runtime/status', !!schema.paths['/v1/runtime/status']);
    assert('info.version', schema.info.version === '3.7.0');
}

console.log('\n[4] 新错误码');
{
    assert('SERVICE_UNAVAILABLE 存在', !!ERROR_CODES.SERVICE_UNAVAILABLE);
    assert('SERVICE_UNAVAILABLE → 503', getErrorStatus('SERVICE_UNAVAILABLE') === 503);
    assert('INVALID_REQUEST_BODY 存在', !!ERROR_CODES.INVALID_REQUEST_BODY);
    assert('INVALID_REQUEST_BODY → 400', getErrorStatus('INVALID_REQUEST_BODY') === 400);
    assert('NOT_FOUND → 404', getErrorStatus('NOT_FOUND') === 404);
}

console.log(`\n结果: ${passed} passed, ${failed} failed\n`);
process.exit(failed > 0 ? 1 : 0);
