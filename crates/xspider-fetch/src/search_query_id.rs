//! 搜索端点的 queryId 自愈（`docs/02-X-DOMAIN-NOTES.md` §A3）。
//!
//! 搜索的 queryId 失效频率比其他端点高（X 改版时它最先变），而**失效的表现是 404**。
//! 自愈流程：搜索页 → `main.<hash>.js` → 从 bundle 里正则提取当前 queryId。
//!
//! # 提取正则必须**锚定 `operationName`**
//!
//! 这是 `docs/02` §A3 用返工换来的一条：
//! bundle 里有 `BookmarkSearchTimeline` / `ListSearchTimeline` /
//! `GlobalCommunitiesPostSearchTimeline` 等好几个同名前缀的 operation，
//! 不锚定端点名就会命中外形相同但**排在前面**的变体，于是"自愈"成功但请求仍然 404
//! ——那比不自愈更难排查。
//!
//! # 与上游的三处差异（都是刻意的，且都有实测依据）
//!
//! 1. 上游用一次性开关 `refreshAttempted` 保证只自愈一次，失败后**永久不再尝试**。
//!    这里改成**冷却窗口**（默认 5 分钟）：既不 hammer，也不会因为一次网络抖动
//!    就把自愈能力永久关掉；
//! 2. 上游把默认 queryId 与"当前值"分开硬编码。这里只有一个默认值常量 + 缓存；
//! 3. **抓搜索页必须带凭据**。上游注释说用 `cdnHeaders`（只有 UA）——那是错的：
//!    实测未登录访问 `/search` 会被 307 到 `x.com/i/jf/onboarding/web`，
//!    那个页面只有 17KB 且**没有任何 JS bundle 引用**，正则必然失败。
//!    带 cookie 时返回 307KB 的真实 SPA 外壳，里面有 `main.<hash>.js`。

use std::sync::Mutex;
use std::time::{Duration, Instant};

use regex::Regex;
use std::sync::OnceLock;

use xspider_core::error::{XError, XResult};

use crate::endpoints::SEARCH_TIMELINE;

/// 自愈冷却：两次尝试之间的最小间隔。
const REFRESH_COOLDOWN: Duration = Duration::from_secs(300);

/// 搜索页（登录态无关；上游也是不带 cookie 抓的）。
const SEARCH_PAGE_URL: &str = "https://x.com/search?q=from%3Atwitter&src=typed_query&f=live";

/// 备选页面。未登录时 `/search` 会 307 到登录页，个别情况下登录页里没有 bundle 引用
/// （实测偶尔如此），这时退到首页——**我们要的只是"当前的 SPA bundle 是哪个文件"**，
/// 而任何 SPA 页面都指向同一个 bundle。
const FALLBACK_PAGE_URL: &str = "https://x.com/";

fn main_bundle_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"https://abs\.twimg\.com/responsive-web/client-web/main\.[a-f0-9]+\.js")
            .expect("内置正则必须编译通过")
    })
}

/// **锚定 operationName 的 queryId 提取**。见模块头注释：这个锚点是必须的。
fn query_id_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"queryId:"([A-Za-z0-9_-]{20,24})",operationName:"SearchTimeline""#)
            .expect("内置正则必须编译通过")
    })
}

/// 从 bundle 文本里提取 `SearchTimeline` 的 queryId。
///
/// 抽成独立函数是为了能**用真实 bundle 的片段**测它（见本文件末尾的测试）。
pub fn extract_query_id(bundle_text: &str) -> Option<String> {
    query_id_re()
        .captures(bundle_text)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

/// 从页面 HTML 里找 main bundle 的 URL。
pub fn extract_main_bundle_url(html: &str) -> Option<String> {
    main_bundle_re().find(html).map(|m| m.as_str().to_string())
}

/// 抓一个 URL 的文本。第二个参数是"要不要带凭据"——见模块头注释第 3 条。
///
/// 用 `(url, with_credentials)` 而不是只给 url，是因为同一次自愈里两个请求的
/// 凭据策略**必须**不同：页面要带（否则拿不到 SPA 外壳），bundle 不带
/// （公开 CDN，没必要把 cookie 送出去）。
pub type PageFetchFn = std::sync::Arc<
    dyn Fn(
            String,
            bool,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = XResult<String>> + Send>>
        + Send
        + Sync,
>;

/// 当前应使用的 queryId 的持有者。
#[derive(Debug, Default)]
pub struct SearchQueryIdProvider {
    cached: Mutex<Option<String>>,
    last_attempt: Mutex<Option<Instant>>,
}

impl SearchQueryIdProvider {
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前应该用的 queryId：自愈成功过就用新的，否则用默认值。
    pub fn current(&self) -> String {
        self.cached
            .lock()
            .ok()
            .and_then(|c| c.clone())
            .unwrap_or_else(|| SEARCH_TIMELINE.query_id.to_string())
    }

    /// 显式设置（测试与录制用）。
    pub fn set(&self, query_id: impl Into<String>) {
        if let Ok(mut c) = self.cached.lock() {
            *c = Some(query_id.into());
        }
    }

    /// 冷却是否已过（过了才允许再自愈）。
    fn cooldown_elapsed(&self) -> bool {
        let mut last = match self.last_attempt.lock() {
            Ok(l) => l,
            Err(_) => return false,
        };
        match *last {
            Some(at) if at.elapsed() < REFRESH_COOLDOWN => false,
            _ => {
                *last = Some(Instant::now());
                true
            }
        }
    }

    /// 尝试自愈。返回 `Some(new_id)` 表示拿到了新值。
    ///
    /// `fetch` 由调用方提供（走 `HttpStack`，带 UA、按 CDN 配额过闸门）。
    pub async fn refresh(&self, fetch: PageFetchFn) -> XResult<Option<String>> {
        if !self.cooldown_elapsed() {
            tracing::debug!("search queryId 自愈还在冷却期内，跳过");
            return Ok(None);
        }

        let mut bundle_url = None;
        let mut last_error = None;
        // 页面**必须带凭据**（模块头注释第 3 条），bundle 不带
        for page in [SEARCH_PAGE_URL, FALLBACK_PAGE_URL] {
            match fetch(page.to_string(), true).await {
                Ok(html) => match extract_main_bundle_url(&html) {
                    Some(url) => {
                        bundle_url = Some(url);
                        break;
                    }
                    // 这个页面里没有 bundle 引用 → 试下一个，而不是立刻失败
                    None => last_error = Some(format!("{page} 里没有 main bundle 引用")),
                },
                Err(e) => last_error = Some(format!("抓 {page} 失败：{e}")),
            }
        }
        let bundle_url = bundle_url.ok_or_else(|| {
            XError::parse(
                "search_query_id.page",
                format!(
                    "找不到 main bundle 的 URL（X 可能改了前端结构）：{}",
                    last_error.unwrap_or_else(|| "无可用页面".to_string())
                ),
            )
        })?;
        let bundle = fetch(bundle_url, false).await?;
        let query_id = extract_query_id(&bundle).ok_or_else(|| {
            XError::parse(
                "search_query_id.bundle",
                "main bundle 里找不到 SearchTimeline 的 queryId（锚点可能是 operationName 的形态变了）",
            )
        })?;

        tracing::info!(query_id = %query_id, "search queryId 自愈成功");
        self.set(query_id.clone());
        Ok(Some(query_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 录制下来的真实 bundle 片段（不是手写样本）。
    ///
    /// 见 `fixtures/search_timeline/query_id_source.json` 的 `note`：
    /// 只含匹配点附近的片段，不含个人数据。
    fn fixture() -> serde_json::Value {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/search_timeline/query_id_source.json"
        );
        let text = std::fs::read_to_string(path).expect(
            "缺少 fixtures/search_timeline/query_id_source.json：             用 XSPIDER_LIVE=1 的录制测试生成",
        );
        serde_json::from_str(&text).expect("fixture 不合法")
    }

    fn field(name: &str) -> String {
        fixture()[name]
            .as_str()
            .unwrap_or_else(|| panic!("fixture 缺少 {name}"))
            .to_string()
    }

    #[test]
    fn extracts_the_id_from_a_real_bundle_excerpt() {
        assert!(
            field("captured_at").starts_with("20"),
            "fixture 必须带采集日期"
        );
        assert_eq!(
            extract_query_id(&field("excerpt")).as_deref(),
            Some(field("expected_query_id").as_str()),
            "真实 bundle 片段里必须能提取到当前 queryId"
        );
    }

    /// **锚点回归测试**：没有锚点就会匹配到兄弟 operation。
    ///
    /// 这条测试用的是**真实存在**的兄弟（例如 `ListSearchTimeline`），
    /// 而不是我编的字符串——所以它证明的是"真会发生"，而不是"理论上可能"。
    #[test]
    fn a_real_sibling_operation_would_be_matched_without_the_anchor() {
        let sibling_name = field("sibling_operation");
        let sibling_query_id = field("sibling_query_id");
        assert_ne!(
            sibling_name, "<无>",
            "fixture 里应该存在一个含 SearchTimeline 的兄弟 operation，否则这条测试失去意义"
        );
        assert_ne!(
            sibling_query_id,
            field("expected_query_id"),
            "兄弟的 queryId 必须与目标不同"
        );

        // 1) 兄弟自己的片段：锚定后**不该**匹配（它的 operationName 不是 SearchTimeline）
        let sibling_excerpt = field("sibling_excerpt");
        if !sibling_excerpt.is_empty() {
            assert_eq!(
                extract_query_id(&sibling_excerpt),
                None,
                "锚定 operationName 后不该匹配到 {sibling_name}"
            );
        }

        // 2) 未锚定的朴素做法会先命中**另一个** queryId —— 这就是当初踩的坑
        let naive = field("unanchored_first_query_id");
        assert_ne!(
            naive,
            field("expected_query_id"),
            "若未锚定的首个匹配恰好就是目标，这条测试就没意义了"
        );

        // 3) 把锚点文字改掉 → 必须提取不到
        let broken =
            field("excerpt").replace("operationName:\"SearchTimeline\"", "operationName:\"Xxx\"");
        assert_eq!(
            extract_query_id(&broken),
            None,
            "去掉锚点后不该还能匹配——说明正则没有真正锚定 operationName"
        );
    }

    #[test]
    fn provider_falls_back_to_the_default_until_healed() {
        let p = SearchQueryIdProvider::new();
        assert_eq!(p.current(), SEARCH_TIMELINE.query_id);
        p.set("auLkqtmHqYEpRvflfvLhyQ");
        assert_eq!(p.current(), "auLkqtmHqYEpRvflfvLhyQ");
    }

    #[test]
    fn cooldown_blocks_immediate_repeat_attempts() {
        let p = SearchQueryIdProvider::new();
        assert!(p.cooldown_elapsed(), "首次允许尝试");
        assert!(!p.cooldown_elapsed(), "紧接着的第二次必须被冷却挡住");
        assert!(!p.cooldown_elapsed(), "冷却期内一直挡住");
    }

    #[test]
    fn bundle_url_is_extracted_from_the_page() {
        let html = r#"<script src="https://abs.twimg.com/responsive-web/client-web/main.a1b2c3d4.js"></script>"#;
        assert_eq!(
            extract_main_bundle_url(html).as_deref(),
            Some("https://abs.twimg.com/responsive-web/client-web/main.a1b2c3d4.js")
        );
        assert_eq!(extract_main_bundle_url("<html></html>"), None);
    }
}
