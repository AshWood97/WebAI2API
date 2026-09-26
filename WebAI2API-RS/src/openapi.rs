//! OpenAPI 3.0.3 文档，结构对应原 `src/server/api/openai/openapi.js`。
//! model 枚举在每次请求时注入当前模型 ID 列表。

use serde_json::{json, Value};

pub fn build_schema(model_ids: &[String], version: &str) -> Value {
    let enum_or_absent = |ids: &[String]| -> Value {
        if ids.is_empty() {
            Value::Null
        } else {
            json!(ids)
        }
    };
    let model_enum = enum_or_absent(model_ids);

    let mut schema = json!({
        "openapi": "3.0.3",
        "info": {
            "title": "WebAI2API",
            "description": "Web AI to OpenAI-compatible API via Camoufox browser automation. Also exposes Anthropic Messages API (dual protocol) inspired by WebModel. Includes health/runtime endpoints inspired by WebAI-to-API.",
            "version": version
        },
        "servers": [{ "url": "http://localhost:3000", "description": "Local default" }],
        "security": [{ "bearerAuth": [] }],
        "paths": {
            "/health": { "get": { "tags": ["System"], "summary": "Process liveness", "security": [],
                "responses": { "200": { "description": "Service is alive", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/HealthResponse" } } } } } } },
            "/ready": { "get": { "tags": ["System"], "summary": "Runtime readiness", "security": [],
                "responses": { "200": { "description": "Service is ready", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ReadyResponse" } } } }, "503": { "description": "Not ready (safe mode / login mode / pool init failed)" } } } },
            "/openapi.json": { "get": { "tags": ["System"], "summary": "OpenAPI schema", "security": [],
                "responses": { "200": { "description": "OpenAPI 3.0 document" } } } },
            "/docs": { "get": { "tags": ["System"], "summary": "Human-readable API catalog page",
                "description": "Self-contained HTML that loads /openapi.json. Requires Bearer auth when server.auth is set.",
                "responses": { "200": { "description": "HTML documentation page" } } } },
            "/v1/models": { "get": { "tags": ["OpenAI"], "summary": "List available models",
                "responses": { "200": { "description": "Model list", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ModelList" } } } } } } },
            "/v1/chat/completions": { "post": { "tags": ["OpenAI"], "summary": "OpenAI-compatible chat completions",
                "requestBody": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ChatCompletionRequest" } } } },
                "responses": { "200": { "description": "Chat completion or SSE stream" }, "400": { "description": "Invalid request" }, "429": { "description": "Queue full / rate limited" }, "503": { "description": "Browser not ready / safe mode" } } } },
            "/v1/stateless/chat/completions": { "post": { "tags": ["OpenAI", "Stateless"], "summary": "Stateless chat completions (client-owned history)",
                "description": "Inspired by WebAI-to-API stateless API. Client owns full history; server does not rely on conversation continuation.",
                "requestBody": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ChatCompletionRequest" } } } },
                "responses": { "200": { "description": "Chat completion or SSE stream" }, "400": { "description": "Invalid request" }, "429": { "description": "Queue full" }, "503": { "description": "Service unavailable" } } } },
            "/v1/messages": { "post": { "tags": ["Anthropic"], "summary": "Anthropic Messages API (dual protocol)",
                "description": "Compatible with Claude Code and Anthropic SDK. Inspired by WebModel.",
                "requestBody": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/AnthropicMessageRequest" } } } },
                "responses": { "200": { "description": "Anthropic message or SSE stream" }, "400": { "description": "Invalid request" }, "429": { "description": "Rate limited" }, "503": { "description": "Overloaded / unavailable" } } } },
            "/v1/auth/status": { "get": { "tags": ["Runtime"], "summary": "Browser / worker authentication status",
                "responses": { "200": { "description": "Auth status of configured workers" } } } },
            "/v1/runtime/status": { "get": { "tags": ["Runtime"], "summary": "Runtime diagnostics",
                "responses": { "200": { "description": "Runtime status payload" } } } },
            "/v1/providers": { "get": { "tags": ["Runtime"], "summary": "Provider / adapter / worker status",
                "description": "Inspired by WebModel /webmodel/providers",
                "responses": { "200": { "description": "Provider status list" } } } },
            "/v1/cookies": { "get": { "tags": ["OpenAI"], "summary": "Export browser cookies",
                "parameters": [
                    { "name": "name", "in": "query", "schema": { "type": "string" }, "description": "Worker/instance name" },
                    { "name": "domain", "in": "query", "schema": { "type": "string" }, "description": "Filter by domain" }
                ],
                "responses": { "200": { "description": "Cookie payload" } } } }
        },
        "components": {
            "securitySchemes": { "bearerAuth": { "type": "http", "scheme": "bearer", "description": "Value of server.auth from config.yaml" } },
            "schemas": {
                "HealthResponse": { "type": "object", "properties": {
                    "status": { "type": "string", "example": "ok" }, "uptime": { "type": "number" },
                    "version": { "type": "string" }, "timestamp": { "type": "string", "format": "date-time" } } },
                "ReadyResponse": { "type": "object", "properties": {
                    "ready": { "type": "boolean" }, "safeMode": { "type": "boolean" }, "loginMode": { "type": "boolean" },
                    "poolReady": { "type": "boolean" }, "reason": { "type": "string", "nullable": true } } },
                "ModelList": { "type": "object", "properties": {
                    "object": { "type": "string", "example": "list" },
                    "data": { "type": "array", "items": { "type": "object", "properties": {
                        "id": { "type": "string" },
                        "object": { "type": "string", "example": "model" },
                        "owned_by": { "type": "string" },
                        "type": { "type": "string", "enum": ["text", "image", "video"] },
                        "image_policy": { "type": "string", "enum": ["optional", "required", "forbidden"] } } } } } },
                "ChatCompletionRequest": { "type": "object", "required": ["messages"], "properties": {
                    "model": { "type": "string" },
                    "messages": { "type": "array", "items": { "type": "object", "required": ["role"], "properties": {
                        "role": { "type": "string", "enum": ["system", "user", "assistant"] },
                        "content": { "oneOf": [{ "type": "string" }, { "type": "array", "items": { "type": "object" } }] } } } },
                    "stream": { "type": "boolean", "default": false },
                    "reasoning": { "type": "boolean", "description": "Request reasoning/thinking extraction when supported" } } },
                "AnthropicMessageRequest": { "type": "object", "required": ["model", "messages"], "properties": {
                    "model": { "type": "string" },
                    "messages": { "type": "array", "items": { "type": "object", "required": ["role", "content"], "properties": {
                        "role": { "type": "string", "enum": ["user", "assistant"] },
                        "content": { "oneOf": [{ "type": "string" }, { "type": "array", "items": { "type": "object" } }] } } } },
                    "system": { "oneOf": [{ "type": "string" }, { "type": "array", "items": { "type": "object" } }] },
                    "max_tokens": { "type": "integer" },
                    "stream": { "type": "boolean", "default": false } } }
            }
        },
        "tags": [
            { "name": "System", "description": "Health / readiness / schema" },
            { "name": "OpenAI", "description": "OpenAI-compatible endpoints" },
            { "name": "Anthropic", "description": "Anthropic Messages dual protocol" },
            { "name": "Stateless", "description": "Client-owned-history endpoints" },
            { "name": "Runtime", "description": "Diagnostics / auth / providers" }
        ]
    });

    // JS 版在模型列表非空时才注入 enum，空时该键不存在
    if !model_ids.is_empty() {
        schema["components"]["schemas"]["ModelList"]["properties"]["data"]["items"]["properties"]
            ["id"]["enum"] = model_enum.clone();
        schema["components"]["schemas"]["ChatCompletionRequest"]["properties"]["model"]["enum"] =
            model_enum.clone();
        schema["components"]["schemas"]["AnthropicMessageRequest"]["properties"]["model"]["enum"] =
            model_enum;
    }
    schema
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_shape() {
        let s = build_schema(&["gpt".into()], "3.10.0");
        assert_eq!(s["openapi"], "3.0.3");
        assert_eq!(s["info"]["version"], "3.10.0");
        for path in [
            "/health",
            "/ready",
            "/v1/messages",
            "/v1/stateless/chat/completions",
            "/v1/providers",
            "/v1/runtime/status",
            "/v1/cookies",
        ] {
            assert!(s["paths"].get(path).is_some(), "{path} 缺失");
        }
        assert_eq!(
            s["components"]["schemas"]["ChatCompletionRequest"]["properties"]["model"]["enum"][0],
            "gpt"
        );
        let empty = build_schema(&[], "3.0.0");
        assert!(
            empty["components"]["schemas"]["ChatCompletionRequest"]["properties"]["model"]
                .get("enum")
                .is_none()
        );
    }
}
