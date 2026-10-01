//! RPC 包络：**两种传输（HTTP / stdio）共用同一份解析与派发**。
//!
//! 与 `xspider_call` 的关系：cdylib 的 `xspider_call(method, json_in)` 返回
//! `{"result":...}` 或 `{"error":...}`；这里的请求体把 `method` 与 `params` 分开装，
//! **响应包络形状完全一致**。所以外壳在两种形态之间切换时，只需要换传输方式。
//!
//! 请求体（`params`、`id`、`token` 都可省）：
//!
//! ```json
//! { "id": 1, "method": "fetch.get_user", "params": { "screen_name": "jack" } }
//! ```
//!
//! 响应：
//!
//! ```json
//! { "id": 1, "result": { "user": { ... } } }
//! { "id": 1, "error": { "code": "rate_limited", "retry_after_s": 120 } }
//! ```
//!
//! `id` 是**传输层**的流水号（stdio 下用来配对请求/响应），不属于契约载荷；
//! 契约载荷只有 `result` / `error`。

use serde::Deserialize;
use serde_json::Value;

use xspider::Engine;
use xspider_core::cancel::CancelToken;
use xspider_core::error::XError;

/// 传输层（sidecar 独有）的 method。**不在契约的 method 表里**——
/// cdylib 形态没有"关掉宿主进程"这种事（`docs/CONTRACT.md` §传输层）。
pub const METHOD_SHUTDOWN: &str = "system.shutdown";

#[derive(Debug, Clone, Deserialize)]
pub struct RpcRequest {
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
    /// aria2 风格的便利字段：允许把 token 放进 body，方便 `curl -d` 直接调。
    #[serde(default)]
    pub token: Option<String>,
}

/// 一次处理的结果：要不要顺手关掉自己。
#[derive(Debug, Clone, PartialEq)]
pub struct Handled {
    pub response: Value,
    pub shutdown: bool,
}

impl Handled {
    fn respond(response: Value) -> Self {
        Self {
            response,
            shutdown: false,
        }
    }
}

/// 解析请求体；失败时返回「已经可以回给调用方」的错误包络。
pub fn parse_request(body: &str) -> Result<RpcRequest, Value> {
    serde_json::from_str::<RpcRequest>(body).map_err(|e| {
        error_response(
            None,
            &XError::invalid_request(format!("请求体不是合法 JSON 或缺少 method：{e}")),
        )
    })
}

/// 鉴权。token 可以放在 header（`Authorization: Bearer x` / `X-XSpider-Token: x`）或 body。
///
/// 每次运行随机生成 token 的意义：**即使本机其他进程也不该随便调用**
/// （`docs/03-FFI-SIGNING-PACKAGING.md` §3）。
pub fn authorize(
    req: &RpcRequest,
    headers: &[(String, String)],
    expected: &str,
) -> Result<(), Value> {
    let from_header = headers.iter().find_map(|(k, v)| {
        if k.eq_ignore_ascii_case("x-xspider-token") {
            Some(v.clone())
        } else if k.eq_ignore_ascii_case("authorization") {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
                .map(str::to_string)
        } else {
            None
        }
    });
    let provided = from_header.or_else(|| req.token.clone());
    match provided {
        Some(t) if constant_time_eq(t.as_bytes(), expected.as_bytes()) => Ok(()),
        _ => Err(error_response(
            req.id.as_ref(),
            &XError::invalid_request(
                "鉴权失败：请在 X-XSpider-Token / Authorization 头或 body 的 token 字段里带上正确 token",
            ),
        )),
    }
}

/// 定长比较，避免用响应时间一点点试出 token。
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// 派发一次调用。**不 panic、不返回空**：任何问题都变成结构化错误包络。
pub async fn dispatch(engine: &Engine, req: &RpcRequest, cancel: &CancelToken) -> Handled {
    if req.method == METHOD_SHUTDOWN {
        return Handled {
            response: ok_response(req.id.as_ref(), serde_json::json!({ "ok": true })),
            shutdown: true,
        };
    }
    let params = req.params.clone().unwrap_or(Value::Null);
    match engine.call_with_cancel(&req.method, &params, cancel).await {
        Ok(result) => Handled::respond(ok_response(req.id.as_ref(), result)),
        Err(e) => Handled::respond(error_response(req.id.as_ref(), &e)),
    }
}

pub fn ok_response(id: Option<&Value>, result: Value) -> Value {
    with_id(id, serde_json::json!({ "result": result }))
}

pub fn error_response(id: Option<&Value>, err: &XError) -> Value {
    with_id(id, serde_json::json!({ "error": err.to_object() }))
}

fn with_id(id: Option<&Value>, mut body: Value) -> Value {
    if let (Some(id), Some(map)) = (id, body.as_object_mut()) {
        // 把 id 放到最前面，人眼读日志更顺
        let mut out = serde_json::Map::new();
        out.insert("id".to_string(), id.clone());
        for (k, v) in map.iter() {
            out.insert(k.clone(), v.clone());
        }
        return Value::Object(out);
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn headers(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn parses_a_minimal_request() {
        let req = parse_request(r#"{"method":"net.status"}"#).unwrap();
        assert_eq!(req.method, "net.status");
        assert!(req.id.is_none());
        assert!(req.params.is_none());
    }

    #[test]
    fn malformed_body_yields_a_structured_error_not_a_panic() {
        let err = parse_request("{oops").unwrap_err();
        assert_eq!(err["error"]["code"], "invalid_request");
    }

    #[test]
    fn missing_method_is_rejected() {
        let err = parse_request(r#"{"params":{}}"#).unwrap_err();
        assert_eq!(err["error"]["code"], "invalid_request");
    }

    #[test]
    fn accepts_token_from_either_header_or_body() {
        let req = parse_request(r#"{"method":"net.status","token":"s3cret"}"#).unwrap();
        assert!(authorize(&req, &headers(&[]), "s3cret").is_ok());
        assert!(authorize(&req, &headers(&[("X-XSpider-Token", "s3cret")]), "s3cret").is_ok());
        assert!(authorize(
            &req,
            &headers(&[("Authorization", "Bearer s3cret")]),
            "s3cret"
        )
        .is_ok());
        assert!(authorize(&req, &headers(&[("x-xspider-token", "s3cret")]), "s3cret").is_ok());
    }

    #[test]
    fn rejects_wrong_or_missing_token() {
        let req = parse_request(r#"{"method":"net.status","id":7}"#).unwrap();
        let err = authorize(&req, &headers(&[]), "s3cret").unwrap_err();
        assert_eq!(err["error"]["code"], "invalid_request");
        // 错误响应里要回带 id，否则 stdio 下无法配对
        assert_eq!(err["id"], 7);
        assert!(authorize(&req, &headers(&[("X-XSpider-Token", "nope")]), "s3cret").is_err());
        // 长度不同也不能通过
        assert!(authorize(
            &req,
            &headers(&[("X-XSpider-Token", "s3cret-longer")]),
            "s3cret"
        )
        .is_err());
    }

    #[test]
    fn id_is_echoed_and_kept_first() {
        let v = ok_response(Some(&json!(1)), json!({"ok":true}));
        let printed = v.to_string();
        assert!(printed.starts_with(r#"{"id":1,"#), "{printed}");
        // 没有 id 就不要凭空造一个
        let v = ok_response(None, json!({"ok":true}));
        assert!(v.get("id").is_none());
    }
}
