//! `fetch.tweet_detail`：一条推文 + 它的回复树。
//!
//! 上游没有这个端点（`api.ts` 里没有），权威参照是 Swift 版的
//! `getTweet` / `extractFocalTweet` / `extractReplyNodes`。那里的实测注释记了两个坑，
//! 本文件都照做：
//!
//! 1. **focal 不能走"要媒体"的解析路径**。用带 `requireMedia` 的解析时，
//!    无媒体的 focal 会被过滤掉，于是退化分支返回"第一条有媒体的推文"——
//!    那往往是评论区里带图的广告或评论，表现为**详情弹出来的是别人的推文**。
//!    所以 focal 按 entryId 直接取，不经过媒体筛选。
//! 2. **`data.tweetResult` 这条老路径不存在了**。当前线上只有
//!    `data.threaded_conversation_with_injections_v2.instructions`；
//!    老路径保留为兜底，但不要以为它一定在。
//!
//! 另外两条来自 `docs/02-X-DOMAIN-NOTES.md`：
//!
//! - §C2：孤儿的 `is_partial_parent = true` 且**不能被丢弃**；
//! - §C4：「评论的评论」只有贴主的，是 X 服务端行为，**不是解析 bug**，
//!   所以不要为"回复树看起来很短"改解析。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use xspider_core::cancel::CancelToken;
use xspider_core::error::{XError, XResult};
use xspider_core::paging::SeenIds;

use crate::calls::{RawCall, ENDPOINT_TWEET_DETAIL};
use crate::post::{parse_post, unwrap_visibility, Post};
use crate::timeline::{
    all_entries, bottom_cursor, collect_entry, instructions_at, missing_instructions, Collected,
    TimelineOptions,
};
use crate::FetchClient;

/// 一条回复。
///
/// 用 `#[serde(flatten)]` 把 `Post` 的字段摊平——外壳处理回复和处理推文是同一套代码，
/// 多一层 `{post: {...}}` 只会让每个访问点都多一次解包。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    #[serde(flatten)]
    pub post: Post,
    /// 本回复在回复谁（父推文 id）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// **父不在本页**（孤儿）。这类条目不能丢，否则前端会出现"回复了某人但找不到上下文"
    /// 的断链（`docs/02` §C2）。
    pub is_partial_parent: bool,
}

impl Reply {
    pub fn is_partial_parent(&self) -> bool {
        self.is_partial_parent
    }
}

/// `fetch.tweet_detail` 的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TweetDetail {
    /// 被点开的那条推文。
    pub focal: Post,
    /// 评论区（已过滤广告、去重、排除 focal 自身）。
    pub replies: Vec<Reply>,
    /// 目前 X **不返回**详情的翻页游标（`docs/02` §C4）；若哪天返回了就带出来。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

impl FetchClient {
    /// 取推文详情。
    pub async fn tweet_detail(&self, id: &str, cancel: &CancelToken) -> XResult<TweetDetail> {
        let call = RawCall::TweetDetail { id };
        let body = self.call_json(call, cancel).await?;
        parse_tweet_detail(&body, id.trim())
    }
}

/// 从响应里解析 focal 与回复树。
///
/// 拆成独立函数，便于用真实 fixture 直接测。
pub fn parse_tweet_detail(body: &Value, focal_id: &str) -> XResult<TweetDetail> {
    let instructions = instructions_at(
        body,
        &[
            // 当前线上路径
            &[
                "data",
                "threaded_conversation_with_injections_v2",
                "instructions",
            ],
            // 老路径，保留为兜底
            &["data", "tweetResult", "result", "timeline", "instructions"],
        ],
    )
    .ok_or_else(|| missing_instructions(ENDPOINT_TWEET_DETAIL, body))?;

    let focal = extract_focal(instructions, focal_id).ok_or_else(|| XError::Parse {
        context: format!("{ENDPOINT_TWEET_DETAIL}.focal"),
        message: format!(
            "回复树里找不到 id={focal_id} 的 focal 推文（entryId 应该是 tweet-{focal_id}）。\
             它可能已被删除或转为不可见；也可能 X 改了 entryId 的形态。"
        ),
        endpoint: Some(ENDPOINT_TWEET_DETAIL.to_string()),
    })?;

    // 回复：用 entry 型遍历（**同一套广告过滤与去重**，docs/02 §C1 的第三个入口）
    let mut collected = Collected::default();
    let mut seen = SeenIds::new();
    for entry in all_entries(instructions) {
        collect_entry(
            &mut collected,
            &mut seen,
            entry,
            TimelineOptions {
                // 回复里纯文字很常见，不能因为"要媒体"把它们筛掉
                require_media: false,
                include_retweets: false,
            },
        );
    }

    // focal 自己不是回复（Swift 版也显式排除，否则它会出现在评论列表里）
    let mut replies: Vec<Post> = collected
        .posts
        .into_iter()
        .filter(|p| p.id != focal.id)
        .collect();

    // 孤儿标记：父既不是 focal，也不在本页 → 标 partial，但**保留**
    let page_ids: Vec<String> = replies.iter().map(|p| p.id.clone()).collect();
    let mut out = Vec::with_capacity(replies.len());
    for post in replies.drain(..) {
        let parent_id = post.in_reply_to_id.clone();
        let is_partial_parent = match &parent_id {
            None => false,
            Some(pid) => pid != &focal.id && !page_ids.contains(pid),
        };
        out.push(Reply {
            post,
            parent_id,
            is_partial_parent,
        });
    }

    Ok(TweetDetail {
        focal,
        replies: out,
        cursor: bottom_cursor(instructions),
    })
}

/// 取 focal。
///
/// 两条路，顺序是刻意的：
/// 1. **entryId 精确命中** `tweet-<id>`（最常见的形态，一次比较就够）；
/// 2. 退路：扫**所有** entry 的 `rest_id` 比对。
///
/// 退路刻意不看 entryId 的形状——它存在的意义正是"entryId 形态变了"这种情况。
/// `rest_id` 唯一，所以扫全部不可能"认错" focal；而按 entryId 前缀猜
/// 会在形态变化时静默返回 `not_found`，把外壳的"推文不存在"文案变成一个谎
/// （Swift 版的退路只认 `tweet` 前缀，这里放宽是刻意的）。
fn extract_focal(instructions: &[Value], focal_id: &str) -> Option<Post> {
    let entries = all_entries(instructions);
    let wanted = format!("tweet-{focal_id}");

    if let Some(entry) = entries
        .iter()
        .find(|e| e.get("entryId").and_then(Value::as_str) == Some(wanted.as_str()))
    {
        if let Some(post) = result_of_entry(entry).and_then(parse_post) {
            return Some(post);
        }
    }

    for entry in entries {
        let nested = entry
            .get("content")
            .and_then(|c| c.get("items"))
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let candidates = result_of_entry(entry)
            .into_iter()
            .chain(nested.iter().filter_map(|item| {
                item.get("item")
                    .and_then(|i| i.get("itemContent"))
                    .and_then(|ic| ic.get("tweet_results"))
                    .and_then(|t| t.get("result"))
            }));
        for result in candidates {
            if unwrap_visibility(result)
                .get("rest_id")
                .and_then(Value::as_str)
                == Some(focal_id)
            {
                if let Some(post) = parse_post(result) {
                    return Some(post);
                }
            }
        }
    }
    None
}

fn result_of_entry(entry: &Value) -> Option<&Value> {
    entry
        .get("content")
        .and_then(|c| c.get("itemContent"))
        .and_then(|ic| ic.get("tweet_results"))
        .and_then(|t| t.get("result"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn result(id: &str, reply_to: Option<(&str, &str)>) -> Value {
        let mut legacy = json!({
            "full_text": format!("post {id}"),
            "created_at": "Wed Sep 30 12:34:56 +0000 2009",
            "entities": { "media": [ { "type": "photo", "id_str": "m",
                                       "media_url_https": "https://pbs.twimg.com/media/a.jpg" } ] }
        });
        if let Some((pid, pscreen)) = reply_to {
            legacy["in_reply_to_status_id_str"] = json!(pid);
            legacy["in_reply_to_screen_name"] = json!(pscreen);
        }
        json!({
            "rest_id": id,
            "legacy": legacy,
            "core": { "user_results": { "result": { "rest_id": "1", "legacy": {
                "screen_name": "u", "name": "U", "profile_image_url_https": "https://x/a.jpg" } } } }
        })
    }

    fn tweet_entry(id: &str, reply_to: Option<(&str, &str)>) -> Value {
        json!({
            "entryId": format!("tweet-{id}"),
            "content": { "itemContent": { "tweet_results": { "result": result(id, reply_to) } } }
        })
    }

    /// 真实响应的形态：focal 在 entries 里，回复散在 tweet-* 与 conversationthread-* 里。
    fn real_shaped_body() -> Value {
        json!({
            "data": {
                "threaded_conversation_with_injections_v2": {
                    "instructions": [{
                        "type": "TimelineAddEntries",
                        "entries": [
                            tweet_entry("100", None),
                            tweet_entry("101", Some(("100", "focal_user"))),
                            { "entryId": "promoted-1", "content": { "itemContent": {
                                "promotedMetadata": { "advertiser_results": {} } } } },
                            { "entryId": "conversationthread-200", "content": { "items": [
                                { "item": { "itemContent": { "tweet_results": {
                                    "result": result("200", Some(("100", "focal_user"))) } } } },
                                { "item": { "itemContent": { "tweet_results": {
                                    "result": result("201", Some(("999", "someone_absent"))) } } } }
                            ] } },
                            tweet_entry("100", None), // 重复（focal 又出现一次）
                            { "entryId": "cursor-bottom-1", "content": { "cursorType": "Bottom", "value": "CUR" } }
                        ]
                    }]
                }
            }
        })
    }

    #[test]
    fn extracts_focal_and_replies_from_a_real_shaped_response() {
        let detail = parse_tweet_detail(&real_shaped_body(), "100").expect("应能解析");
        assert_eq!(detail.focal.id, "100");
        assert_eq!(detail.focal.full_text, "post 100");
        // focal 自己不能出现在回复里
        assert!(detail.replies.iter().all(|r| r.post.id != "100"));

        let ids: Vec<&str> = detail.replies.iter().map(|r| r.post.id.as_str()).collect();
        assert_eq!(ids, vec!["101", "200", "201"], "顺序必须保持（去重不重排）");
    }

    #[test]
    fn promoted_entries_are_filtered_from_replies() {
        // docs/02 §C1：回复节点也是广告入口之一
        let detail = parse_tweet_detail(&real_shaped_body(), "100").unwrap();
        assert!(
            detail.replies.iter().all(|r| r.post.id != "ad"),
            "广告不该出现在回复里"
        );
    }

    #[test]
    fn duplicate_ids_are_deduped() {
        let detail = parse_tweet_detail(&real_shaped_body(), "100").unwrap();
        let mut ids: Vec<&str> = detail.replies.iter().map(|r| r.post.id.as_str()).collect();
        let before = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), before, "回复里不该有重复 id");
    }

    /// docs/02 §C2：父不在本页的孤儿要标 `is_partial_parent`，**且不能丢**。
    #[test]
    fn orphan_replies_are_marked_partial_but_kept() {
        let detail = parse_tweet_detail(&real_shaped_body(), "100").unwrap();
        let orphan = detail
            .replies
            .iter()
            .find(|r| r.post.id == "201")
            .expect("孤儿回复不能被丢掉");
        assert!(orphan.is_partial_parent, "父 999 不在本页 → 必须标 partial");
        assert_eq!(orphan.parent_id.as_deref(), Some("999"));

        let direct = detail.replies.iter().find(|r| r.post.id == "101").unwrap();
        assert!(!direct.is_partial_parent, "父就是 focal → 不算孤儿");
        assert_eq!(direct.parent_id.as_deref(), Some("100"));
        // 「回复 @xxx」用被回复者，不是本条作者（docs/02 §C2）
        assert_eq!(
            direct.post.in_reply_to_screen_name.as_deref(),
            Some("focal_user")
        );
    }

    #[test]
    fn focal_without_media_is_still_found() {
        // Swift 版记的坑：带 requireMedia 的解析会丢掉无媒体的 focal，
        // 于是详情里弹出的是别人的推文。这里 focal 必须原样取到。
        let mut body = real_shaped_body();
        body["data"]["threaded_conversation_with_injections_v2"]["instructions"][0]["entries"][0]
            ["content"]["itemContent"]["tweet_results"]["result"]["legacy"]["entities"] = json!({});
        let detail = parse_tweet_detail(&body, "100").unwrap();
        assert_eq!(detail.focal.id, "100");
        assert!(
            detail.focal.medias.is_empty(),
            "无媒体的 focal 照样是 focal"
        );
    }

    #[test]
    fn falls_back_to_rest_id_when_entry_id_shape_changes() {
        let body = json!({
            "data": { "threaded_conversation_with_injections_v2": { "instructions": [{
                "type": "TimelineAddEntries",
                "entries": [ { "entryId": "unknown-shape", "content": { "itemContent": {
                    "tweet_results": { "result": result("100", None) } } } } ]
            }] } }
        });
        // entryId 对不上时，退路要能按 rest_id 找到
        let detail = parse_tweet_detail(&body, "100").expect("必须能兜住");
        assert_eq!(detail.focal.id, "100");
    }

    #[test]
    fn missing_focal_is_a_parse_error_naming_the_id() {
        let body = json!({
            "data": { "threaded_conversation_with_injections_v2": { "instructions": [{
                "type": "TimelineAddEntries",
                "entries": [ tweet_entry("1", None) ]
            }] } }
        });
        let err = parse_tweet_detail(&body, "100").unwrap_err();
        assert_eq!(err.code(), xspider_core::error::ErrorCode::Parse);
        assert!(err.to_string().contains("100"), "{err}");
        assert_eq!(err.endpoint(), Some(ENDPOINT_TWEET_DETAIL));
    }

    #[test]
    fn missing_instructions_is_reported_with_context() {
        let body = json!({ "data": { "wat": true } });
        let err = parse_tweet_detail(&body, "1").unwrap_err();
        assert_eq!(err.code(), xspider_core::error::ErrorCode::Parse);
        assert!(err.to_string().contains(ENDPOINT_TWEET_DETAIL), "{err}");
    }

    #[test]
    fn old_tweet_result_path_is_still_supported_as_a_fallback() {
        let body = json!({
            "data": { "tweetResult": { "result": { "timeline": { "instructions": [{
                "type": "TimelineAddEntries",
                "entries": [ tweet_entry("100", None) ]
            }] } } } }
        });
        let detail = parse_tweet_detail(&body, "100").expect("老路径要能兜住");
        assert_eq!(detail.focal.id, "100");
    }

    #[test]
    fn reply_serializes_flattened() {
        let detail = parse_tweet_detail(&real_shaped_body(), "100").unwrap();
        let json = serde_json::to_value(&detail.replies[0]).unwrap();
        // 摊平：id 直接在顶层，而不是 {post: {id: ...}}
        assert!(json.get("id").is_some(), "{json}");
        assert!(json.get("post").is_none(), "{json}");
        assert!(json.get("is_partial_parent").is_some(), "{json}");
    }
}
