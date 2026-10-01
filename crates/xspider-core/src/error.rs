//! 结构化错误分类。
//!
//! 契约形状（见 `docs/01-ARCHITECTURE.md` §6）：
//!
//! ```json
//! { "error": { "code": "rate_limited", "message": "...", "retry_after_s": 120, "endpoint": "user_by_screen_name" } }
//! ```
//!
//! **调用方必须按 `code` 做判断，不许匹配 `message` 文案。**
//! 一个真实的反面教材：下载侧曾用匹配引擎输出的错误文案（`"exit 3"` / `"404"`）来决定能否重试，
//! 引擎一换就全错。`code` 是枚举，`message` 只给人看。

use serde::{Deserialize, Serialize};

/// 错误码枚举。新增能力只会**追加**码值，不改已有语义（见 `docs/CONTRACT.md`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// 请求本身不合法：未知 method、缺字段、字段类型错。**重试无意义**。
    InvalidRequest,
    /// 凭据失效 / 被登出。外壳应提示重新登录，不要重试。
    Unauthorized,
    /// 限流。带 `retry_after_s`。组件内部退避/挂起，外壳展示。
    RateLimited,
    /// 用户/推文不存在。
    NotFound,
    /// 服务端错误（含 404 与 queryId 失效的区别，见 docs/02 §A2）。
    Upstream,
    /// 解析失败 = X 可能改版了。**这是最需要报警的一类。**
    Parse,
    /// 网络 / 代理 / DNS。
    Transport,
    /// 调用方取消。
    Cancelled,
    /// 我们自己的 bug（不该出现；出现即须修）。
    Internal,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::InvalidRequest => "invalid_request",
            ErrorCode::Unauthorized => "unauthorized",
            ErrorCode::RateLimited => "rate_limited",
            ErrorCode::NotFound => "not_found",
            ErrorCode::Upstream => "upstream",
            ErrorCode::Parse => "parse",
            ErrorCode::Transport => "transport",
            ErrorCode::Cancelled => "cancelled",
            ErrorCode::Internal => "internal",
        }
    }
}

/// 契约里的 `error` 对象。字段与 `contract/xspider.schema.json` 的 `$defs/error` 一一对应。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorObject {
    pub code: ErrorCode,
    /// 给人看的说明（可含端点/字段上下文）。**不可用于逻辑判断。**
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_s: Option<u64>,
    /// 仅 `upstream`：上游 HTTP 状态码。用来区分 404 与 5xx。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// 仅 `parse`：解析失败的位置（如 `entries[0].content.itemContent`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// 逻辑端点名（如 `user_by_screen_name`）。**不是** URL 路径，不含 queryId。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// 仅 `transport`：`{ "kind": "timeout" | "connect" | "tls" | "body" | "proxy" | "fixture" | "other" }`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
}

/// 内核统一错误类型。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum XError {
    #[error("{message}")]
    InvalidRequest { message: String },

    #[error("{message}")]
    Unauthorized {
        message: String,
        endpoint: Option<String>,
    },

    #[error("限流：请等待 {retry_after_s}s 后重试（端点 {endpoint:?}）")]
    RateLimited {
        retry_after_s: u64,
        endpoint: Option<String>,
    },

    #[error("{message}")]
    NotFound {
        message: String,
        endpoint: Option<String>,
    },

    #[error("上游返回 HTTP {status}：{message}")]
    Upstream {
        status: u16,
        message: String,
        endpoint: Option<String>,
    },

    #[error("解析失败（{context}）：{message}")]
    Parse {
        context: String,
        message: String,
        endpoint: Option<String>,
    },

    #[error("网络错误（{kind}）：{message}")]
    Transport {
        kind: &'static str,
        message: String,
        endpoint: Option<String>,
    },

    #[error("调用方已取消")]
    Cancelled,

    #[error("{message}")]
    Internal { message: String },
}

impl XError {
    pub fn invalid_request(message: impl Into<String>) -> Self {
        XError::InvalidRequest {
            message: message.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        XError::Internal {
            message: message.into(),
        }
    }

    pub fn parse(context: impl Into<String>, message: impl Into<String>) -> Self {
        XError::Parse {
            context: context.into(),
            message: message.into(),
            endpoint: None,
        }
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        XError::Unauthorized {
            message: message.into(),
            endpoint: None,
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        XError::NotFound {
            message: message.into(),
            endpoint: None,
        }
    }

    pub fn transport(kind: &'static str, message: impl Into<String>) -> Self {
        XError::Transport {
            kind,
            message: message.into(),
            endpoint: None,
        }
    }

    pub fn upstream(status: u16, message: impl Into<String>) -> Self {
        XError::Upstream {
            status,
            message: message.into(),
            endpoint: None,
        }
    }

    /// 挂上逻辑端点名（错误里带上下文，X 改版时才能一眼定位）。
    pub fn with_endpoint(self, endpoint: impl Into<String>) -> Self {
        let endpoint = Some(endpoint.into());
        match self {
            XError::Unauthorized { message, .. } => XError::Unauthorized { message, endpoint },
            XError::RateLimited { retry_after_s, .. } => XError::RateLimited {
                retry_after_s,
                endpoint,
            },
            XError::NotFound { message, .. } => XError::NotFound { message, endpoint },
            XError::Upstream {
                status, message, ..
            } => XError::Upstream {
                status,
                message,
                endpoint,
            },
            XError::Parse {
                context, message, ..
            } => XError::Parse {
                context,
                message,
                endpoint,
            },
            XError::Transport { kind, message, .. } => XError::Transport {
                kind,
                message,
                endpoint,
            },
            other => other,
        }
    }

    pub fn code(&self) -> ErrorCode {
        match self {
            XError::InvalidRequest { .. } => ErrorCode::InvalidRequest,
            XError::Unauthorized { .. } => ErrorCode::Unauthorized,
            XError::RateLimited { .. } => ErrorCode::RateLimited,
            XError::NotFound { .. } => ErrorCode::NotFound,
            XError::Upstream { .. } => ErrorCode::Upstream,
            XError::Parse { .. } => ErrorCode::Parse,
            XError::Transport { .. } => ErrorCode::Transport,
            XError::Cancelled => ErrorCode::Cancelled,
            XError::Internal { .. } => ErrorCode::Internal,
        }
    }

    pub fn endpoint(&self) -> Option<&str> {
        match self {
            XError::Unauthorized { endpoint, .. }
            | XError::RateLimited { endpoint, .. }
            | XError::NotFound { endpoint, .. }
            | XError::Upstream { endpoint, .. }
            | XError::Parse { endpoint, .. }
            | XError::Transport { endpoint, .. } => endpoint.as_deref(),
            _ => None,
        }
    }

    pub fn to_object(&self) -> ErrorObject {
        let message = self.to_string();
        let endpoint = self.endpoint().map(str::to_string);
        match self {
            XError::InvalidRequest { .. } => ErrorObject {
                code: ErrorCode::InvalidRequest,
                message,
                retry_after_s: None,
                status: None,
                context: None,
                endpoint,
                detail: None,
            },
            XError::Unauthorized { .. } => ErrorObject {
                code: ErrorCode::Unauthorized,
                message,
                retry_after_s: None,
                status: None,
                context: None,
                endpoint,
                detail: None,
            },
            XError::RateLimited { retry_after_s, .. } => ErrorObject {
                code: ErrorCode::RateLimited,
                message,
                retry_after_s: Some(*retry_after_s),
                status: None,
                context: None,
                endpoint,
                detail: None,
            },
            XError::NotFound { .. } => ErrorObject {
                code: ErrorCode::NotFound,
                message,
                retry_after_s: None,
                status: None,
                context: None,
                endpoint,
                detail: None,
            },
            XError::Upstream { status, .. } => ErrorObject {
                code: ErrorCode::Upstream,
                message,
                retry_after_s: None,
                status: Some(*status),
                context: None,
                endpoint,
                detail: None,
            },
            XError::Parse { context, .. } => ErrorObject {
                code: ErrorCode::Parse,
                message,
                retry_after_s: None,
                status: None,
                context: Some(context.clone()),
                endpoint,
                detail: None,
            },
            XError::Transport { kind, .. } => ErrorObject {
                code: ErrorCode::Transport,
                message,
                retry_after_s: None,
                status: None,
                context: None,
                endpoint,
                detail: Some(serde_json::json!({ "kind": kind })),
            },
            XError::Cancelled => ErrorObject {
                code: ErrorCode::Cancelled,
                message,
                retry_after_s: None,
                status: None,
                context: None,
                endpoint: None,
                detail: None,
            },
            XError::Internal { .. } => ErrorObject {
                code: ErrorCode::Internal,
                message,
                retry_after_s: None,
                status: None,
                context: None,
                endpoint: None,
                detail: None,
            },
        }
    }
}

/// 成功包络：`{"result": ...}`
pub fn ok_envelope(result: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "result": result })
}

/// 失败包络：`{"error": {...}}`
pub fn err_envelope(err: &XError) -> serde_json::Value {
    serde_json::json!({ "error": err.to_object() })
}

/// 把 `XError` 序列化成一个 JSON 字符串（C ABI / sidecar 共用）。
pub fn err_envelope_string(err: &XError) -> String {
    err_envelope(err).to_string()
}

pub type XResult<T> = Result<T, XError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_json_shape_matches_contract() {
        let err = XError::RateLimited {
            retry_after_s: 120,
            endpoint: Some("user_by_screen_name".into()),
        };
        let v = err_envelope(&err);
        assert_eq!(v["error"]["code"], "rate_limited");
        assert_eq!(v["error"]["retry_after_s"], 120);
        assert_eq!(v["error"]["endpoint"], "user_by_screen_name");
        // 与 code 无关的字段不该出现（schema 用 additionalProperties:false 卡这个）
        assert!(v["error"].get("status").is_none());
        assert!(v["error"].get("context").is_none());
    }

    #[test]
    fn upstream_carries_status_and_parse_carries_context() {
        let up = XError::upstream(503, "boom");
        let v = err_envelope(&up);
        assert_eq!(v["error"]["code"], "upstream");
        assert_eq!(v["error"]["status"], 503);

        let pe = XError::parse("entries[0].content.itemContent", "missing");
        let v = err_envelope(&pe);
        assert_eq!(v["error"]["code"], "parse");
        assert_eq!(v["error"]["context"], "entries[0].content.itemContent");
    }

    #[test]
    fn transport_carries_kind_in_detail() {
        let te = XError::transport("timeout", "10s 未响应");
        let v = err_envelope(&te);
        assert_eq!(v["error"]["code"], "transport");
        assert_eq!(v["error"]["detail"]["kind"], "timeout");
    }

    #[test]
    fn error_object_roundtrips_through_json() {
        let err = XError::NotFound {
            message: "找不到该用户".into(),
            endpoint: Some("user_by_screen_name".into()),
        };
        let obj = err.to_object();
        let s = serde_json::to_string(&obj).unwrap();
        let back: ErrorObject = serde_json::from_str(&s).unwrap();
        assert_eq!(obj, back);
    }
}
