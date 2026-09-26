//! 错误码表，逐条对应原 `src/server/errors.js`；本模块同时集中定义
//! 各模块的 thiserror 错误枚举（Display 文案与替换前的字符串逐字一致，
//! 保证 HTTP 响应与日志零变化）。

use std::path::PathBuf;

/// 引擎桥（bridge.rs）错误：远端消息（Remote）原样透传桥进程的文案。
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("连接引擎桥失败 ({path}): {source}")]
    Connect {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("引擎桥连接已断开")]
    Disconnected,
    #[error(transparent)]
    Serialize(#[from] serde_json::Error),
    #[error("引擎桥写入通道已关闭")]
    WriteClosed,
    #[error("引擎桥响应通道关闭")]
    ReplyChannelClosed,
    #[error("引擎桥调用超时: {method}")]
    Timeout { method: String },
    /// 桥进程在 error 字段里返回的原始消息，逐字透传。
    #[error("{0}")]
    Remote(String),
    #[error("启动引擎桥失败: {source}")]
    Spawn {
        #[source]
        source: std::io::Error,
    },
    #[error("引擎桥未在 10 秒内创建 socket")]
    SocketNotCreated,
    #[error("引擎桥未在 30 秒内就绪")]
    ReadyTimeout,
    #[error("引擎桥就绪通道关闭")]
    ReadyChannelClosed,
}

/// 请求历史（history.rs）错误：rusqlite / io 错误原样透传（原实现为 e.to_string()）。
#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error("历史记录数据库未初始化")]
    NotInitialized,
    #[error("历史记录数据库任务失败: {source}")]
    TaskFailed {
        #[from]
        source: tokio::task::JoinError,
    },
    #[error(transparent)]
    Database(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("不是 data URI")]
    NotDataUri,
    #[error("data URI 缺少 base64")]
    NotBase64,
    #[error("base64 解码失败: {source}")]
    Base64 {
        #[from]
        source: base64::DecodeError,
    },
}

/// 每日统计（stats.rs）错误。
#[derive(Debug, thiserror::Error)]
pub enum StatsError {
    #[error("统计任务失败: {source}")]
    TaskFailed {
        #[from]
        source: tokio::task::JoinError,
    },
    #[error("start 日期无效")]
    InvalidStartDate,
    #[error("end 日期无效")]
    InvalidEndDate,
}

/// 单实例锁（instance_lock.rs）错误。
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("创建数据目录失败: {source}")]
    CreateDataDir {
        #[source]
        source: std::io::Error,
    },
    #[error("单实例锁创建失败: {source}")]
    LockCreate {
        #[source]
        source: std::io::Error,
    },
    #[error("单实例锁写入失败: {source}")]
    LockWrite {
        #[source]
        source: std::io::Error,
    },
    #[error(
        "检测到另一个 WebAI2API 正在运行 (PID {pid})。为避免端口冲突和浏览器无限重启，本次拒绝启动"
    )]
    AlreadyRunning { pid: i32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorDetail {
    pub message: &'static str,
    pub status: u16,
    pub error_type: &'static str,
}

/// 返回 (默认中文消息, HTTP 状态码, OpenAI 错误类型)。未知码回退 500。
pub fn error_detail(code: &str) -> ErrorDetail {
    match code {
        "UNAUTHORIZED" => ErrorDetail {
            message: "未授权（Token 无效或缺失）",
            status: 401,
            error_type: "invalid_request_error",
        },
        "BROWSER_NOT_INITIALIZED" => ErrorDetail {
            message: "浏览器未初始化",
            status: 503,
            error_type: "server_error",
        },
        "SERVER_BUSY" => ErrorDetail {
            message: "服务器繁忙（队列已满）",
            status: 429,
            error_type: "rate_limit_error",
        },
        "NO_MESSAGES" => ErrorDetail {
            message: "请求参数缺少 messages",
            status: 400,
            error_type: "invalid_request_error",
        },
        "NO_USER_MESSAGES" => ErrorDetail {
            message: "messages 中缺少 role=user 的消息",
            status: 400,
            error_type: "invalid_request_error",
        },
        "TOO_MANY_IMAGES" => ErrorDetail {
            message: "图片数量超过限制",
            status: 400,
            error_type: "invalid_request_error",
        },
        "INVALID_MODEL" => ErrorDetail {
            message: "模型无效/后端不支持",
            status: 400,
            error_type: "invalid_request_error",
        },
        "IMAGE_REQUIRED" => ErrorDetail {
            message: "该模型需要参考图",
            status: 400,
            error_type: "invalid_request_error",
        },
        "IMAGE_FORBIDDEN" => ErrorDetail {
            message: "该模型不支持图片输入",
            status: 400,
            error_type: "invalid_request_error",
        },
        "RECAPTCHA" => ErrorDetail {
            message: "触发人机验证（reCAPTCHA）",
            status: 403,
            error_type: "server_error",
        },
        "INTERNAL_ERROR" => ErrorDetail {
            message: "服务器内部错误",
            status: 500,
            error_type: "server_error",
        },
        "GENERATION_FAILED" => ErrorDetail {
            message: "图片生成失败",
            status: 502,
            error_type: "server_error",
        },
        "SERVICE_UNAVAILABLE" => ErrorDetail {
            message: "服务暂时不可用",
            status: 503,
            error_type: "server_error",
        },
        "INVALID_REQUEST_BODY" => ErrorDetail {
            message: "请求体无效",
            status: 400,
            error_type: "invalid_request_error",
        },
        "NOT_FOUND" => ErrorDetail {
            message: "资源不存在",
            status: 404,
            error_type: "invalid_request_error",
        },
        _ => ErrorDetail {
            message: "未知错误",
            status: 500,
            error_type: "server_error",
        },
    }
}

/// Anthropic 错误类型按 HTTP 状态映射（respond.js sendAnthropicError）。
pub fn anthropic_error_type(status: u16) -> &'static str {
    match status {
        400 => "invalid_request_error",
        401 => "authentication_error",
        403 => "permission_error",
        404 => "not_found_error",
        429 => "rate_limit_error",
        503 => "overloaded_error",
        _ => "api_error",
    }
}

/// 适配器层错误码（errors.js ADAPTER_ERRORS），桥返回的 code 原样透传。
pub const ADAPTER_ERRORS: [&str; 10] = [
    "PAGE_CLOSED",
    "PAGE_CRASHED",
    "PAGE_INVALID",
    "NETWORK_ERROR",
    "TIMEOUT_ERROR",
    "HTTP_ERROR",
    "RATE_LIMITED",
    "CAPTCHA_REQUIRED",
    "AUTH_REQUIRED",
    "CONTENT_BLOCKED",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_table_matches_js() {
        let cases = [
            ("UNAUTHORIZED", 401u16, "invalid_request_error"),
            ("SERVER_BUSY", 429, "rate_limit_error"),
            ("RECAPTCHA", 403, "server_error"),
            ("GENERATION_FAILED", 502, "server_error"),
            ("SERVICE_UNAVAILABLE", 503, "server_error"),
            ("NOT_FOUND", 404, "invalid_request_error"),
            ("NO_SUCH_CODE", 500, "server_error"),
        ];
        for (code, status, ty) in cases {
            let d = error_detail(code);
            assert_eq!(d.status, status, "{code}");
            assert_eq!(d.error_type, ty, "{code}");
        }
        assert_eq!(error_detail("IMAGE_REQUIRED").message, "该模型需要参考图");
    }

    #[test]
    fn anthropic_type_map() {
        assert_eq!(anthropic_error_type(401), "authentication_error");
        assert_eq!(anthropic_error_type(503), "overloaded_error");
        assert_eq!(anthropic_error_type(502), "api_error");
        assert_eq!(ADAPTER_ERRORS.len(), 10);
    }
}
