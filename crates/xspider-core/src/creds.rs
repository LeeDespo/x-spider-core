//! 凭据注入与保管（铁律 5：**只进不出**）。
//!
//! - cookie / `ct0` 由外壳通过 `auth.set_cookie` 注入；
//! - **不落盘、不打日志、不回传**：`Debug` 被手工实现为脱敏形式，
//!   访问器是 `pub(crate)`，外部 crate（含 xspider-fetch）拿不到明文；
//! - 契约里也不会出现 cookie：`net.status` 只回报状态，不回报凭据。

use std::fmt;

use crate::error::{XError, XResult};

/// 一次会话的凭据。**不要**给它派生 `Debug`/`Serialize`。
#[derive(Clone)]
pub struct Credentials {
    cookie: String,
    csrf: String,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 连长度都不透露
        f.write_str("Credentials(<redacted>)")
    }
}

impl Credentials {
    /// 从 cookie 串构造。`csrf` 缺省时从 cookie 里的 `ct0` 推导
    /// （X 的 `X-Csrf-Token` 头就等于 cookie 里的 `ct0`，见 docs/02 §A5）。
    pub fn from_cookie(cookie: impl Into<String>, csrf: Option<String>) -> XResult<Self> {
        let cookie = cookie.into();
        let parsed_ct0 = parse_cookie_value(&cookie, "ct0");
        let csrf = match csrf.filter(|s| !s.trim().is_empty()) {
            Some(explicit) => explicit,
            None => parsed_ct0.clone().ok_or_else(|| {
                XError::invalid_request(
                    "cookie 里没有 ct0，且未显式提供 csrf：这不是一个有效的登录态 cookie",
                )
            })?,
        };
        if cookie.trim().is_empty() {
            return Err(XError::invalid_request("cookie 不能为空"));
        }
        Ok(Self { cookie, csrf })
    }

    pub(crate) fn cookie(&self) -> &str {
        &self.cookie
    }

    pub(crate) fn csrf(&self) -> &str {
        &self.csrf
    }
}

/// 从 `k=v; k2=v2` 形式的 cookie 串里取值。上游 `utils/cookie.ts` 的 `parseCookie` 语义。
pub fn parse_cookie_value(cookie: &str, key: &str) -> Option<String> {
    cookie.split(';').find_map(|part| {
        let (k, v) = part.split_once('=')?;
        (k.trim() == key).then(|| v.trim().to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const COOKIE: &str = "auth_token=aaa; ct0=bbb; twid=u%3D123";

    #[test]
    fn derives_csrf_from_ct0() {
        let c = Credentials::from_cookie(COOKIE, None).unwrap();
        assert_eq!(c.csrf(), "bbb");
        assert_eq!(c.cookie(), COOKIE);
    }

    #[test]
    fn explicit_csrf_wins() {
        let c = Credentials::from_cookie(COOKIE, Some("zzz".into())).unwrap();
        assert_eq!(c.csrf(), "zzz");
    }

    #[test]
    fn rejects_cookie_without_ct0() {
        let err = Credentials::from_cookie("auth_token=aaa", None).unwrap_err();
        assert_eq!(err.code(), crate::error::ErrorCode::InvalidRequest);
    }

    #[test]
    fn rejects_empty_cookie() {
        assert!(Credentials::from_cookie("", Some("x".into())).is_err());
    }

    #[test]
    fn debug_never_leaks_the_cookie() {
        let c = Credentials::from_cookie(COOKIE, None).unwrap();
        let printed = format!("{c:?}");
        assert!(!printed.contains("aaa"));
        assert!(!printed.contains("bbb"));
        assert_eq!(printed, "Credentials(<redacted>)");
    }

    #[test]
    fn parse_cookie_value_tolerates_spaces_and_missing_keys() {
        assert_eq!(
            parse_cookie_value("  a=1 ;  ct0 = 2 ", "ct0").as_deref(),
            Some("2")
        );
        assert_eq!(parse_cookie_value("a=1", "ct0"), None);
    }
}
