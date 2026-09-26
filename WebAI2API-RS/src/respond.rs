//! 响应构造，字节级对应原 `src/server/respond.js`。
//! JSON 序列化与 JS `JSON.stringify` 对齐：对象键按插入顺序、无多余空白。

use crate::errors::{anthropic_error_type, error_detail};
use serde_json::{json, Value};

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// `data: <json>\n\n`
pub fn sse_event(payload: &Value) -> String {
    format!("data: {}\n\n", payload)
}

pub fn sse_done() -> &'static str {
    "data: [DONE]\n\n"
}

/// 心跳：comment 模式是注释行，content 模式是空 delta chunk（respond.js:46-66）。
pub fn heartbeat(mode: &str, model_name: Option<&str>) -> String {
    if mode == "comment" {
        ":keepalive\n\n".to_string()
    } else {
        let ms = now_ms();
        let chunk = json!({
            "id": format!("chatcmpl-{ms}"),
            "object": "chat.completion.chunk",
            "created": ms / 1000,
            "model": model_name.unwrap_or("default-model"),
            "choices": [{ "index": 0, "delta": { "content": "" }, "finish_reason": null }]
        });
        sse_event(&chunk)
    }
}

/// OpenAI 错误体 `{error:{message,type,code}}`。
pub fn openai_error_body(code: &str, message: Option<&str>) -> Value {
    let d = error_detail(code);
    json!({
        "error": {
            "message": message.unwrap_or(d.message),
            "type": d.error_type,
            "code": if code.is_empty() { "INTERNAL_ERROR" } else { code }
        }
    })
}

/// 非流式 chat completion（respond.js:112-132）。
pub fn chat_completion(content: &str, model_name: Option<&str>, reasoning: Option<&str>) -> Value {
    let ms = now_ms();
    let mut message = json!({ "role": "assistant", "content": content });
    if let Some(r) = reasoning.filter(|s| !s.is_empty()) {
        message["reasoning_content"] = json!(r);
    }
    json!({
        "id": format!("chatcmpl-{ms}"),
        "object": "chat.completion",
        "created": ms / 1000,
        "model": model_name.unwrap_or("default-model"),
        "choices": [{ "index": 0, "message": message, "finish_reason": "stop" }]
    })
}

/// 流式 chunk（respond.js:142-159）。
pub fn chat_completion_chunk(
    content: &str,
    model_name: Option<&str>,
    finish_reason: Option<&str>,
    reasoning: Option<&str>,
) -> Value {
    let ms = now_ms();
    let mut delta = json!({ "content": content });
    if let Some(r) = reasoning.filter(|s| !s.is_empty()) {
        delta["reasoning_content"] = json!(r);
    }
    json!({
        "id": format!("chatcmpl-{ms}"),
        "object": "chat.completion.chunk",
        "created": ms / 1000,
        "model": model_name.unwrap_or("default-model"),
        "choices": [{
            "index": 0,
            "delta": delta,
            "finish_reason": finish_reason.unwrap_or("stop")
        }]
    })
}

fn tokens_of(text: &str) -> u64 {
    std::cmp::max(1, (text.chars().count() / 4) as u64)
}

/// Anthropic 非流式消息（respond.js:170-190）。
pub fn anthropic_message(
    content: &str,
    model_name: Option<&str>,
    reasoning: Option<&str>,
    input_tokens: u64,
) -> Value {
    let mut blocks = Vec::new();
    if let Some(r) = reasoning.filter(|s| !s.is_empty()) {
        blocks.push(json!({ "type": "thinking", "thinking": r }));
    }
    blocks.push(json!({ "type": "text", "text": content }));
    json!({
        "id": format!("msg_{}", radix36(now_ms())),
        "type": "message",
        "role": "assistant",
        "model": model_name.unwrap_or("default-model"),
        "content": blocks,
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {
            "input_tokens": if input_tokens == 0 { tokens_of(content) } else { input_tokens },
            "output_tokens": tokens_of(content)
        }
    })
}

/// Anthropic 流式事件序列，返回可直接写出的 SSE 文本（respond.js:199-275）。
pub fn anthropic_events(
    content: &str,
    model_name: Option<&str>,
    reasoning: Option<&str>,
) -> String {
    let msg_id = format!("msg_{}", radix36(now_ms()));
    let model = model_name.unwrap_or("default-model");
    let ev = |name: &str, data: Value| format!("event: {name}\ndata: {data}\n\n");

    let mut out = ev(
        "message_start",
        json!({
            "type": "message_start",
            "message": {
                "id": msg_id, "type": "message", "role": "assistant", "model": model,
                "content": [], "stop_reason": null, "stop_sequence": null,
                "usage": { "input_tokens": 1, "output_tokens": 0 }
            }
        }),
    );

    let block = |index: u64, kind: &str, field: &str| {
        (
            ev(
                "content_block_start",
                json!({
                    "type": "content_block_start", "index": index,
                    "content_block": { "type": kind, field: "" }
                }),
            ),
            ev(
                "content_block_stop",
                json!({ "type": "content_block_stop", "index": index }),
            ),
        )
    };

    if let Some(r) = reasoning.filter(|s| !s.is_empty()) {
        let (start, stop) = block(0, "thinking", "thinking");
        out.push_str(&start);
        out.push_str(&ev(
            "content_block_delta",
            json!({
                "type": "content_block_delta", "index": 0,
                "delta": { "type": "thinking_delta", "thinking": r }
            }),
        ));
        out.push_str(&stop);
        let (start, stop) = block(1, "text", "text");
        out.push_str(&start);
        out.push_str(&ev(
            "content_block_delta",
            json!({
                "type": "content_block_delta", "index": 1,
                "delta": { "type": "text_delta", "text": content }
            }),
        ));
        out.push_str(&stop);
    } else {
        let (start, stop) = block(0, "text", "text");
        out.push_str(&start);
        out.push_str(&ev(
            "content_block_delta",
            json!({
                "type": "content_block_delta", "index": 0,
                "delta": { "type": "text_delta", "text": content }
            }),
        ));
        out.push_str(&stop);
    }

    out.push_str(&ev(
        "message_delta",
        json!({
            "type": "message_delta",
            "delta": { "stop_reason": "end_turn", "stop_sequence": null },
            "usage": { "output_tokens": tokens_of(content) }
        }),
    ));
    out.push_str(&ev("message_stop", json!({ "type": "message_stop" })));
    out
}

/// Anthropic 错误体（respond.js:286-320）。
pub fn anthropic_error_body(
    code: &str,
    message: Option<&str>,
    status_override: Option<u16>,
) -> (u16, Value) {
    let d = error_detail(code);
    let status = status_override.unwrap_or(d.status);
    let body = json!({
        "type": "error",
        "error": { "type": anthropic_error_type(status), "message": message.unwrap_or(d.message) }
    });
    (status, body)
}

pub fn anthropic_error_event(payload: &Value) -> String {
    format!("event: error\ndata: {payload}\n\n")
}

/// JS `Number#toString(36)` 等价（仅非负整数）。
fn radix36(mut n: u128) -> String {
    if n == 0 {
        return "0".to_string();
    }
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut buf = Vec::new();
    while n > 0 {
        buf.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    buf.reverse();
    String::from_utf8(buf).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_framing() {
        let s = sse_event(&json!({"a": 1}));
        assert_eq!(s, "data: {\"a\":1}\n\n");
        assert_eq!(sse_done(), "data: [DONE]\n\n");
        assert_eq!(heartbeat("comment", None), ":keepalive\n\n");
    }

    #[test]
    fn heartbeat_content_shape() {
        let s = heartbeat("content", Some("gpt"));
        assert!(s.starts_with("data: {"));
        assert!(s.ends_with("\n\n"));
        let json_part = &s[6..s.len() - 2];
        let v: Value = serde_json::from_str(json_part).unwrap();
        assert_eq!(v["object"], "chat.completion.chunk");
        assert_eq!(v["choices"][0]["delta"]["content"], "");
        assert!(v["choices"][0]["finish_reason"].is_null());
        assert_eq!(v["model"], "gpt");
    }

    #[test]
    fn completion_shapes() {
        let c = chat_completion("hi", None, Some("think"));
        assert_eq!(c["object"], "chat.completion");
        assert_eq!(c["choices"][0]["message"]["reasoning_content"], "think");
        assert_eq!(c["choices"][0]["finish_reason"], "stop");

        let k = chat_completion_chunk("hi", None, Some("stop"), None);
        assert!(k["choices"][0]["delta"].get("reasoning_content").is_none());

        let m = anthropic_message("abcd", Some("claude"), None, 0);
        assert_eq!(m["type"], "message");
        assert_eq!(m["usage"]["output_tokens"], 1);
        assert_eq!(m["content"][0]["type"], "text");

        let with_think = anthropic_message("abcd", None, Some("r"), 9);
        assert_eq!(with_think["content"][0]["type"], "thinking");
        assert_eq!(with_think["usage"]["input_tokens"], 9);
    }

    #[test]
    fn anthropic_event_sequence() {
        let ev = anthropic_events("hello", Some("m"), None);
        let names: Vec<&str> = ev
            .lines()
            .filter(|l| l.starts_with("event: "))
            .map(|l| &l[7..])
            .collect();
        assert_eq!(
            names,
            [
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop"
            ]
        );

        let ev2 = anthropic_events("hello", None, Some("reason"));
        let names2: Vec<&str> = ev2
            .lines()
            .filter(|l| l.starts_with("event: "))
            .map(|l| &l[7..])
            .collect();
        assert_eq!(
            names2
                .iter()
                .filter(|n| **n == "content_block_start")
                .count(),
            2
        );

        let (status, body) = anthropic_error_body("SERVER_BUSY", None, None);
        assert_eq!(status, 429);
        assert_eq!(body["error"]["type"], "rate_limit_error");
    }

    #[test]
    fn radix36_matches_js() {
        // (1750000000000).toString(36) === 'r8c8w5og' 的数量级校验：纯数字映射
        assert_eq!(radix36(0), "0");
        assert_eq!(radix36(35), "z");
        assert_eq!(radix36(36), "10");
    }
}
