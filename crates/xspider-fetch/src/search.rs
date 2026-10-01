//! `fetch.search_timeline`：搜索时间线。
//!
//! 三条来自实测的硬性事实（`docs/02-X-DOMAIN-NOTES.md`）：
//!
//! 1. **必须 POST + JSON body**。GET 一律 404，**且这个 404 与 queryId 无关**
//!    （§A2）——实测新旧两个 queryId 用 POST 都返回 200，只有乱写的才 404。
//!    别再把 404 误判成"queryId 失效"去折腾自愈。
//! 2. **queryId 自愈要锚定 `operationName`**（§A3），见 [`crate::search_query_id`]。
//! 3. 日期筛选：`until:` 是**排他**的 → 结束日 +1 天；契约里的日期是用户本地日历日期，
//!    **组件不做时区换算**（§D3）。见 [`xspider_core::xdate::search_date_range`]。
//!
//! 自愈只在**真的** 404 时触发（由 `crate::calls::FetchClient::call_json` 统一处理），
//! 且最多重试一次——避免"越限越试"。

use xspider_core::cancel::CancelToken;
use xspider_core::error::XResult;
use xspider_core::paging::Page;

use crate::calls::{RawCall, DEFAULT_COUNT, ENDPOINT_SEARCH_TIMELINE};
use crate::post::Post;
use crate::timeline::{
    collect_from_modules, finish_page, instructions_at, missing_instructions, TimelineOptions,
};
use crate::FetchClient;

impl FetchClient {
    /// 搜索某用户在某日期区间内的推文（可选只要媒体）。
    ///
    /// - `since` / `until`：`YYYY-MM-DD`，**用户本地日历日期**。`until` 含当天
    ///   （内部 +1 天以适配 X 的排他语义，外壳不要再加一天）；
    /// - `media_only`：服务端 `filter:media`，比"取回来再筛"省请求（也省配额）；
    /// - 返回 `Page<Post>`：**单页 + 游标**，不自动翻页。
    pub async fn search_timeline(
        &self,
        screen_name: &str,
        since: &str,
        until: &str,
        media_only: bool,
        cursor: Option<&str>,
        cancel: &CancelToken,
    ) -> XResult<Page<Post>> {
        let call = RawCall::SearchTimeline {
            screen_name,
            since,
            until,
            media_only,
            cursor,
            count: Some(DEFAULT_COUNT),
        };
        let body = self.call_json(call, cancel).await?;
        parse_search_timeline(&body)
    }
}

/// 解析搜索响应。拆出来便于用真实 fixture 直接测。
pub fn parse_search_timeline(body: &serde_json::Value) -> XResult<Page<Post>> {
    let instructions = instructions_at(
        body,
        &[&[
            "data",
            "search_by_raw_query",
            "search_timeline",
            "timeline",
            "instructions",
        ]],
    )
    .ok_or_else(|| missing_instructions(ENDPOINT_SEARCH_TIMELINE, body))?;

    // 与 UserMedia 同一套 module 型遍历 → 广告过滤与去重自动一致（docs/02 §C1）。
    // 这里不再按媒体筛：`media_only` 已经交给服务端（`filter:media`），
    // 客户端再筛一遍只会把"服务端认为有媒体但 entities 里暂时没有"的条目误杀。
    let collected = collect_from_modules(
        instructions,
        TimelineOptions {
            require_media: false,
            include_retweets: false,
        },
    );
    finish_page(ENDPOINT_SEARCH_TIMELINE, collected, instructions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn post_result(id: &str) -> serde_json::Value {
        json!({
            "rest_id": id,
            "legacy": { "full_text": format!("p{id}"), "created_at": "Wed Sep 30 12:34:56 +0000 2009",
                        "entities": { "media": [ { "type": "photo", "id_str": "m",
                            "media_url_https": "https://pbs.twimg.com/media/a.jpg" } ] } },
            "core": { "user_results": { "result": { "rest_id": "1", "legacy": {
                "screen_name": "u", "name": "U", "profile_image_url_https": "https://x/a.jpg" } } } }
        })
    }

    fn module_entry(items: Vec<(&str, bool)>) -> serde_json::Value {
        let items: Vec<serde_json::Value> = items
            .into_iter()
            .map(|(id, promoted)| {
                let mut item_content = json!({ "tweet_results": { "result": post_result(id) } });
                if promoted {
                    item_content["promotedMetadata"] = json!({ "advertiser_results": {} });
                }
                json!({ "item": { "itemContent": item_content } })
            })
            .collect();
        json!({ "entryId": "search-module", "content": {
            "entryType": "TimelineTimelineModule", "items": items } })
    }

    fn search_body(entries: Vec<serde_json::Value>) -> serde_json::Value {
        json!({
            "data": { "search_by_raw_query": { "search_timeline": { "timeline": {
                "instructions": [ { "type": "TimelineAddEntries", "entries": entries } ]
            } } } }
        })
    }

    #[test]
    fn parses_items_cursor_and_end_from_a_search_response() {
        let body = search_body(vec![
            module_entry(vec![("1", false), ("2", true), ("3", false)]),
            json!({ "entryId": "cursor-bottom-1", "content": {
                "cursorType": "Bottom", "value": "NEXT" } }),
        ]);
        let page = parse_search_timeline(&body).unwrap();
        let ids: Vec<&str> = page.items.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["1", "3"], "广告（id=2）必须被过滤");
        assert_eq!(page.cursor.as_deref(), Some("NEXT"));
        assert!(!page.end);
    }

    #[test]
    fn empty_search_page_reports_end_without_a_cursor() {
        // docs/02 §B4：0 条 → cursor null
        let body = search_body(vec![json!({ "entryId": "cursor-bottom-1", "content": {
            "cursorType": "Bottom", "value": "STILL" } })]);
        let page = parse_search_timeline(&body).unwrap();
        assert!(page.items.is_empty());
        assert!(page.end);
        assert!(page.cursor.is_none(), "空页不能回吐游标");
    }

    #[test]
    fn response_without_search_instructions_is_a_parse_error() {
        let body = json!({ "data": { "something_else": true } });
        let err = parse_search_timeline(&body).unwrap_err();
        assert_eq!(err.code(), xspider_core::error::ErrorCode::Parse);
        assert!(err.to_string().contains(ENDPOINT_SEARCH_TIMELINE), "{err}");
    }
}
