//! URL 查询值编码。
//!
//! 用 `encodeURIComponent` 等价集（字母数字 + `-_.!~*'()`），也就是上游浏览器里
//! `fetch`/axios 对 query 值的行为。**不要"顺手"换成 `serde_urlencoded`/form 编码**：
//! 那些会放过 `*`、`~` 之外的差异，而 X 对 `variables` 的解析是逐字敏感的
//! （`docs/02-X-DOMAIN-NOTES.md` §0 移植纪律）。
//!
//! 这一步还有一层作用：它保证最终 URL 是纯 ASCII，从而在经代理（CONNECT）时不会
//! 因为非 ASCII 而被中途改写。

const HEX: &[u8; 16] = b"0123456789ABCDEF";

fn is_unreserved(b: u8) -> bool {
    matches!(b,
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9'
        | b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')')
}

/// 把查询值编码成可安全放进 URL 的形式（空格 → `%20`，不是 `+`）。
pub fn encode_query_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + value.len() / 2);
    for &b in value.as_bytes() {
        if is_unreserved(b) {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0F) as usize] as char);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_encode_uri_component_for_json() {
        let json = r#"{"screen_name":"jack","withSafetyModeUserFields":true}"#;
        assert_eq!(
            encode_query_value(json),
            "%7B%22screen_name%22%3A%22jack%22%2C%22withSafetyModeUserFields%22%3Atrue%7D"
        );
    }

    #[test]
    fn keeps_unreserved_and_escapes_the_rest() {
        assert_eq!(
            encode_query_value("a-b_c.d!e~f*g'h(i)"),
            "a-b_c.d!e~f*g'h(i)"
        );
        assert_eq!(encode_query_value("a b"), "a%20b");
        assert_eq!(encode_query_value("a+b"), "a%2Bb");
        assert_eq!(encode_query_value("a&b=c"), "a%26b%3Dc");
        assert_eq!(encode_query_value("/i/api"), "%2Fi%2Fapi");
    }

    #[test]
    fn non_ascii_is_percent_encoded_as_utf8() {
        assert_eq!(encode_query_value("中"), "%E4%B8%AD");
        // 结果必须是纯 ASCII，否则经代理会被改写
        assert!(encode_query_value("中文用户名😀").is_ascii());
    }

    #[test]
    fn empty_stays_empty() {
        assert_eq!(encode_query_value(""), "");
    }
}
