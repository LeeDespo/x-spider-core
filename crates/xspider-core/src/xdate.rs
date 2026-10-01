//! X 的时间格式 ↔ RFC3339。
//!
//! X 的 `legacy.created_at` 形如 `Wed Sep 30 12:34:56 +0000 2009`（Twitter 老格式）。
//! 契约里一律用 RFC3339 UTC（语言中立、可比较、无歧义），所以在这里一次性转换。
//!
//! **不引入日期库**是刻意的：M0 只需要解析这一种格式，而 M1 的日期筛选（`since`/`until`、
//! 本地时区、排他边界，见 `docs/02` §D3）确实需要真正的时区能力——那时再引入，
//! 并连同那些语义测试一起落地。现在引入只会多一个不可控的依赖面。
//!
//! 注意：这里**只做 UTC 归一化**。把 `until:` 转成"用户选的结束日 + 1 天"、以及
//! 用**本地时区**格式化筛选参数，是 M1 的事（docs/02 §D3），不要在这里顺手做。

/// 解析 Twitter 时间格式（`Wed Sep 30 12:34:56 +0000 2009`）→ RFC3339 UTC。
pub fn parse_x_created_at(raw: &str) -> Option<String> {
    let parts: Vec<&str> = raw.split_whitespace().collect();
    if parts.len() != 6 {
        return None;
    }
    let month = month_from_abbrev(parts[1])?;
    let day: i64 = parts[2].parse().ok()?;
    let mut hms = parts[3].split(':');
    let hour: i64 = hms.next()?.parse().ok()?;
    let minute: i64 = hms.next()?.parse().ok()?;
    let second: i64 = hms.next()?.parse().ok()?;
    if hms.next().is_some() {
        return None;
    }
    let offset = parse_offset(parts[4])?;
    let year: i64 = parts[5].parse().ok()?;

    if !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    let local_days = days_from_civil(year, month, day);
    // 本地时间 - 偏移 = UTC
    let local_secs = local_days * 86_400 + hour * 3600 + minute * 60 + second;
    let utc_secs = local_secs - offset;

    let days = utc_secs.div_euclid(86_400);
    let rem = utc_secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    Some(format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    ))
}

/// `+0000` / `-0700` → 相对 UTC 的秒数。
fn parse_offset(raw: &str) -> Option<i64> {
    let bytes = raw.as_bytes();
    if bytes.len() != 5 {
        return None;
    }
    let sign = match bytes[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let hours: i64 = raw[1..3].parse().ok()?;
    let minutes: i64 = raw[3..5].parse().ok()?;
    if hours > 23 || minutes > 59 {
        return None;
    }
    Some(sign * (hours * 3600 + minutes * 60))
}

fn month_from_abbrev(raw: &str) -> Option<i64> {
    match raw {
        "Jan" => Some(1),
        "Feb" => Some(2),
        "Mar" => Some(3),
        "Apr" => Some(4),
        "May" => Some(5),
        "Jun" => Some(6),
        "Jul" => Some(7),
        "Aug" => Some(8),
        "Sep" => Some(9),
        "Oct" => Some(10),
        "Nov" => Some(11),
        "Dec" => Some(12),
        _ => None,
    }
}

/// 公历 → 天数（Unix 纪元起算）。Howard Hinnant 的 `days_from_civil`。
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// 天数 → 公历。Howard Hinnant 的 `civil_from_days`。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year, m as u32, d as u32)
}

/// 当前 Unix 秒。
pub fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Unix 秒 → `YYYY-MM-DD`（UTC）。
///
/// 只用于给 fixture / 日志打"哪一天"的标签（例如 `captured_at`、`field_changed_<日期>.json`）。
/// **不要拿它做业务日期筛选**：`docs/02` §D3 要求筛选参数用**本地时区**格式化，
/// 那是 M1 要单独落地并有测试的东西。
pub fn format_date_utc(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// 今天（UTC）的 `YYYY-MM-DD`。
pub fn today_utc() -> String {
    format_date_utc(now_unix_secs())
}

// ---------------------------------------------------------------------------
// 日历日期（`YYYY-MM-DD`）：搜索端点的日期筛选用
// ---------------------------------------------------------------------------

/// 解析 `YYYY-MM-DD`，并校验它是**真实存在的日期**（不是 2026-02-30）。
pub fn parse_iso_date(raw: &str) -> Option<(i64, i64, i64)> {
    let raw = raw.trim();
    let mut parts = raw.split('-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let d: i64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // 用"反过来算"校验日期真实性：不存在的日期无法往返
    let days = days_from_civil(y, m, d);
    if civil_from_days(days) != (y, m as u32, d as u32) {
        return None;
    }
    Some((y, m, d))
}

/// `YYYY-MM-DD` 加天数。非法输入返回 `None`（**不要**默默当成今天）。
pub fn add_days(date: &str, days: i64) -> Option<String> {
    let (y, m, d) = parse_iso_date(date)?;
    let shifted = days_from_civil(y, m, d) + days;
    let (ny, nm, nd) = civil_from_days(shifted);
    Some(format!("{ny:04}-{nm:02}-{nd:02}"))
}

/// 校验并归一化（去掉空白）。非法即 `None`。
pub fn normalize_iso_date(raw: &str) -> Option<String> {
    let (y, m, d) = parse_iso_date(raw)?;
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

/// 某个**日历日期区间**转成 X 搜索要的 `since:` / `until:` 值。
///
/// 关键语义（`docs/02-X-DOMAIN-NOTES.md` §D3）：
/// - 契约里的 `since` / `until` 是**用户本地日历上的日期**（`YYYY-MM-DD`），
///   由外壳给定——组件**不做时区换算**，因此也不存在"UTC 差一天"这个坑；
/// - X 的 `until:` 是**排他**的，所以结束日要 **+1 天**，否则"至"当天没内容；
/// - 这个 +1 只在这里做一次。外壳不要再自己加一天（`docs/02` §D3 明确警告过重复加）。
pub fn search_date_range(since: &str, until: &str) -> Option<(String, String)> {
    let since = normalize_iso_date(since)?;
    let until_exclusive = add_days(until, 1)?;
    Some((since, until_exclusive))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_twitter_format() {
        assert_eq!(
            parse_x_created_at("Wed Sep 30 12:34:56 +0000 2009").as_deref(),
            Some("2009-09-30T12:34:56Z")
        );
        // X 的日期里日号可能是空格填充（split_whitespace 天然处理）
        assert_eq!(
            parse_x_created_at("Thu Sep  3 00:00:00 +0000 2020").as_deref(),
            Some("2020-09-03T00:00:00Z")
        );
    }

    #[test]
    fn converts_nonzero_offsets_to_utc() {
        // +0800 的 08:00 = UTC 00:00
        assert_eq!(
            parse_x_created_at("Mon Jan 01 08:00:00 +0800 2024").as_deref(),
            Some("2024-01-01T00:00:00Z")
        );
        // -0700 的 20:00 = 次日 UTC 03:00（跨日进位）
        assert_eq!(
            parse_x_created_at("Mon Jan 01 20:00:00 -0700 2024").as_deref(),
            Some("2024-01-02T03:00:00Z")
        );
    }

    #[test]
    fn handles_leap_years_and_epoch() {
        assert_eq!(
            parse_x_created_at("Thu Jan 01 00:00:00 +0000 1970").as_deref(),
            Some("1970-01-01T00:00:00Z")
        );
        assert_eq!(
            parse_x_created_at("Sat Feb 29 12:00:00 +0000 2020").as_deref(),
            Some("2020-02-29T12:00:00Z")
        );
    }

    #[test]
    fn rejects_garbage_without_panicking() {
        for bad in [
            "",
            "not a date",
            "Wed Sep 30 12:34:56 2009",
            "Wed Xxx 30 12:34:56 +0000 2009",
            "Wed Sep 30 12:34:56 +0000",
            "Wed Sep 99 12:34:56 +0000 2009",
            "Wed Sep 30 25:00:00 +0000 2009",
            "Wed Sep 30 12:34:56 Z000 2009",
        ] {
            assert_eq!(parse_x_created_at(bad), None, "应拒绝：{bad:?}");
        }
    }

    #[test]
    fn days_and_civil_roundtrip() {
        for days in [-10_000i64, -1, 0, 1, 19_000, 100_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(
                days_from_civil(y, i64::from(m), i64::from(d)),
                days,
                "days={days}"
            );
        }
    }

    #[test]
    fn now_is_after_2026() {
        // 防止 SystemTime 方向搞反（这会静默地把所有时间算错）
        assert!(now_unix_secs() > 1_767_225_600, "now={}", now_unix_secs());
    }

    #[test]
    fn format_date_utc_matches_known_days() {
        assert_eq!(format_date_utc(0), "1970-01-01");
        assert_eq!(format_date_utc(1_254_314_096), "2009-09-30");
        assert_eq!(format_date_utc(1_767_225_600), "2026-01-01");
        // 跨日边界：23:59:59 与 00:00:00 必须差一天
        assert_eq!(format_date_utc(86_399), "1970-01-01");
        assert_eq!(format_date_utc(86_400), "1970-01-02");
        // 负值不能 panic（div_euclid）
        assert_eq!(format_date_utc(-1), "1969-12-31");
    }

    #[test]
    fn parse_iso_date_rejects_impossible_dates() {
        assert_eq!(parse_iso_date("2026-01-01"), Some((2026, 1, 1)));
        assert_eq!(parse_iso_date(" 2026-01-01 "), Some((2026, 1, 1)));
        assert_eq!(parse_iso_date("2026-02-30"), None, "2 月没有 30 号");
        assert_eq!(parse_iso_date("2025-02-29"), None, "2025 不是闰年");
        assert_eq!(
            parse_iso_date("2024-02-29"),
            Some((2024, 2, 29)),
            "2024 是闰年"
        );
        assert_eq!(parse_iso_date("2026-13-01"), None);
        assert_eq!(parse_iso_date("2026-01-32"), None);
        assert_eq!(parse_iso_date("2026-1-1"), Some((2026, 1, 1)));
        for bad in ["", "2026", "2026-01", "2026-01-01-01", "abc", "2026/01/01"] {
            assert_eq!(parse_iso_date(bad), None, "应拒绝 {bad:?}");
        }
    }

    #[test]
    fn add_days_handles_month_and_year_rollover() {
        assert_eq!(add_days("2026-01-31", 1).as_deref(), Some("2026-02-01"));
        assert_eq!(add_days("2026-12-31", 1).as_deref(), Some("2027-01-01"));
        assert_eq!(add_days("2024-02-28", 1).as_deref(), Some("2024-02-29"));
        assert_eq!(add_days("2025-02-28", 1).as_deref(), Some("2025-03-01"));
        assert_eq!(add_days("2026-03-01", -1).as_deref(), Some("2026-02-28"));
        assert_eq!(add_days("不是日期", 1), None);
    }

    /// docs/02 §D3：`until` 是排他的，所以结束日必须 +1 天，否则"至"当天没内容。
    #[test]
    fn search_range_makes_until_exclusive() {
        assert_eq!(
            search_date_range("2026-01-01", "2026-01-31"),
            Some(("2026-01-01".to_string(), "2026-02-01".to_string())),
            "结束日必须 +1 天（排他语义）"
        );
        // 单日区间：同一天也要能取到那一天的内容
        assert_eq!(
            search_date_range("2026-05-05", "2026-05-05"),
            Some(("2026-05-05".to_string(), "2026-05-06".to_string()))
        );
        assert_eq!(search_date_range("2026-02-30", "2026-03-01"), None);
    }

    #[test]
    fn today_looks_like_an_iso_date() {
        let today = today_utc();
        assert_eq!(today.len(), 10, "{today}");
        assert_eq!(&today[4..5], "-");
        assert_eq!(&today[7..8], "-");
        assert!(today.starts_with("20"), "{today}");
    }
}
