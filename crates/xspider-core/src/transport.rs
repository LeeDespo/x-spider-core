//! 传输层抽象：**这是离线测试的唯一接缝**。
//!
//! 真网络只是 `Transport` 的一个实现（`Live`）；fixture 回放是另一个（`Replay`）。
//! 于是「解析 + 分页 + 错误分类」这些真正会因 X 改版而红的逻辑，
//! 全部可以在 `cargo test` 默认（不碰网络）下被验证——
//! 这是 `docs/04-TESTING-AND-FIXTURES.md` 第 2 层「fixture 回放」的实现方式。

use crate::error::XError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
    /// 只要响应头，不要响应体。**唯一用途是"问服务端这个文件多大"**
    /// （媒体对象里没有字节数，只能问 CDN，见 `HttpStack::probe_size`）。
    /// 用 HEAD 而不是"发个 GET 再丢掉 body"是有意的：后者遇到不支持 Range 的
    /// 服务器时，必须把整个文件读进内存才知道它有多大。
    Head,
}

impl HttpMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            HttpMethod::Get => "GET",
            HttpMethod::Post => "POST",
            HttpMethod::Head => "HEAD",
        }
    }
}

/// 一次待发请求。请求头里可能含 Cookie，**绝不可整体打日志**。
#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub method: HttpMethod,
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// 请求体。搜索端点是 POST + JSON body（`docs/02` §A2），所以传输层必须支持它。
    pub body: Option<Vec<u8>>,
    /// 是否跟随重定向。
    ///
    /// **默认 false**：`/i/api/` 的请求不该有重定向，跟随只会把"鉴权失败"伪装成
    /// 一次诡异的 200（这是刻意的防线）。
    /// 只有抓公开页面（如搜索页）时才需要打开——未登录访问 `/search` 会 307 到登录页，
    /// 而登录页同样是 SPA 外壳，里面照样有我们要的 bundle 引用。
    pub follow_redirects: bool,
}

impl HttpRequest {
    pub fn new(method: HttpMethod, url: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            headers: Vec::new(),
            body: None,
            follow_redirects: false,
        }
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// 允许跟随重定向（**只给公开页面用**，见字段注释）。
    pub fn follow_redirects(mut self) -> Self {
        self.follow_redirects = true;
        self
    }

    /// 设置 JSON 请求体（同时补上 `Content-Type`，避免调用方忘掉）。
    pub fn json_body(mut self, body: &serde_json::Value) -> Self {
        self.body = Some(body.to_string().into_bytes());
        if !self
            .headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        {
            self.headers
                .push(("Content-Type".to_string(), "application/json".to_string()));
        }
        self
    }

    /// 请求是否带了登录态 cookie（fixture 路由用；不会泄露内容）。
    pub fn has_cookie(&self) -> bool {
        self.headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("cookie") && !v.trim().is_empty())
    }
}

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// 上游的 `Retry-After`（秒数形式）。HTTP-date 形式也支持，但只在能解析时用。
    pub fn retry_after_s(&self) -> Option<u64> {
        let raw = self.header("retry-after")?.trim();
        if let Ok(secs) = raw.parse::<u64>() {
            return Some(secs);
        }
        parse_http_date_secs(raw)
    }
}

/// 解析 RFC 7231 IMF-fixdate（`Sun, 06 Nov 1994 08:49:37 GMT`）→ **绝对** Unix 秒。
///
/// 注意字段顺序与 X 的时间格式**不同**：HTTP-date 是「日 月 年 时:分:秒 GMT」，
/// X 的 `created_at` 是「周 月 日 时:分:秒 偏移 年」（见 `crate::xdate`）。
/// 第一版就是照抄了后者，测试当场红（见 AGENTS.md 踩坑记录）。
pub fn unix_secs_from_http_date(raw: &str) -> Option<u64> {
    let parts: Vec<&str> = raw.split_whitespace().collect();
    if parts.len() != 6 {
        return None;
    }
    let day: u64 = parts[1].parse().ok()?;
    let month = match parts[2] {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year: u64 = parts[3].parse().ok()?;
    let hms: Vec<&str> = parts[4].split(':').collect();
    if hms.len() != 3 {
        return None;
    }
    let hour: u64 = hms[0].parse().ok()?;
    let min: u64 = hms[1].parse().ok()?;
    let sec: u64 = hms[2].parse().ok()?;
    unix_secs_from_utc(year, month, day, hour, min, sec)
}

/// 相对"现在"的秒数（用于 `retry-after`）。已过期返回 0。
fn parse_http_date_secs(raw: &str) -> Option<u64> {
    let target = unix_secs_from_http_date(raw)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some(target.saturating_sub(now))
}

/// 公历 → Unix 秒（Howard Hinnant 的 days_from_civil）。
pub fn unix_secs_from_utc(year: u64, month: u64, day: u64, h: u64, m: u64, s: u64) -> Option<u64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let y = if month <= 2 { year - 1 } else { year } as i64;
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = ((month + 9) % 12) as i64;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    if days < 0 {
        return None;
    }
    Some(days as u64 * 86_400 + h * 3600 + m * 60 + s)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportError {
    /// `timeout` | `connect` | `body` | `fixture` | `other`
    pub kind: &'static str,
    pub message: String,
}

impl TransportError {
    pub fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl From<TransportError> for XError {
    fn from(e: TransportError) -> Self {
        XError::transport(e.kind, e.message)
    }
}

pub type TransportResult = Result<HttpResponse, TransportError>;

/// 传输后端。用枚举而不是 `dyn Trait`，避免为此引入 `async-trait` / `futures` 依赖；
/// 变体都是廉价可克隆的（reqwest::Client 内部是 Arc，fixture 集是 Arc）。
#[derive(Debug, Clone)]
pub enum Transport {
    /// 真网络。
    Live(Box<crate::http::ReqwestTransport>),
    /// fixture 回放（离线，见 `crate::fixture`）。
    Replay(Box<crate::fixture::ReplayTransport>),
}

impl Transport {
    pub async fn execute(&self, req: &HttpRequest) -> TransportResult {
        match self {
            Transport::Live(t) => t.execute(req).await,
            Transport::Replay(t) => t.execute(req).await,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Transport::Live(_) => "live",
            Transport::Replay(_) => "replay",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_body_sets_content_type_once() {
        let req = HttpRequest::new(HttpMethod::Post, "https://x/y")
            .json_body(&serde_json::json!({"a":1}));
        assert_eq!(req.body.as_deref(), Some(br#"{"a":1}"#.as_slice()));
        let cts: Vec<&str> = req
            .headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("content-type"))
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(cts, vec!["application/json"], "不该重复加 Content-Type");

        // 调用方自己设过的 Content-Type 不被覆盖
        let req = HttpRequest::new(HttpMethod::Post, "https://x/y")
            .header("Content-Type", "application/json; charset=utf-8")
            .json_body(&serde_json::json!({}));
        let cts: Vec<&str> = req
            .headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("content-type"))
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(cts.len(), 1);
        assert!(cts[0].contains("charset"));
    }

    #[test]
    fn header_lookup_is_case_insensitive() {
        let r = HttpResponse {
            status: 429,
            headers: vec![("Retry-After".into(), "42".into())],
            body: vec![],
        };
        assert_eq!(r.header("retry-after"), Some("42"));
        assert_eq!(r.retry_after_s(), Some(42));
    }

    #[test]
    fn retry_after_http_date_matches_the_rfc_example() {
        // RFC 7231 §7.1.1.1 的经典样例
        assert_eq!(
            unix_secs_from_http_date("Sun, 06 Nov 1994 08:49:37 GMT"),
            Some(784_111_777)
        );
        // 字段顺序写错（照抄 X 格式）时会解析失败或给出完全不同的值，这里钉住顺序
        assert_eq!(
            unix_secs_from_http_date("Sun, 06 Nov 08:49:37 1994 GMT"),
            None
        );
        assert_eq!(unix_secs_from_http_date("garbage"), None);
        assert_eq!(
            unix_secs_from_http_date("Sun, 06 Xxx 1994 08:49:37 GMT"),
            None
        );
    }

    #[test]
    fn retry_after_http_date_in_the_past_saturates_to_zero() {
        // 已过去的日期 → 0 而不是 panic / 回绕
        let resp = HttpResponse {
            status: 429,
            headers: vec![("Retry-After".into(), "Sun, 06 Nov 1994 08:49:37 GMT".into())],
            body: vec![],
        };
        assert_eq!(resp.retry_after_s(), Some(0));
    }

    #[test]
    fn unix_secs_matches_known_epoch() {
        assert_eq!(unix_secs_from_utc(1970, 1, 1, 0, 0, 0), Some(0));
        assert_eq!(
            unix_secs_from_utc(2009, 9, 30, 12, 34, 56),
            Some(1_254_314_096)
        );
    }
}
