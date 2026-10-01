//! 时间线类端点共用的**指令遍历**，以及四个分页端点。
//!
//! # 为什么要共用一套遍历
//!
//! 上游把同一段逻辑抄了三遍（`getUserMedias` / `getUserTweets` / 搜索），
//! 结果就是 `docs/02-X-DOMAIN-NOTES.md` §C1 记下的那个坑：
//! **广告过滤有三个入口，漏一个就在对应界面露出广告**。
//! 这里把遍历收敛成两个函数（module 型 / entry 型），过滤只写一遍。
//!
//! # 三条语义（都在 `docs/04-TESTING-AND-FIXTURES.md` §3 的清单里）
//!
//! 1. **去重先于筛选**（`docs/02` §D1）——先记 id，再按媒体/转推过滤；
//! 2. **无 `created_at` 的条目放行**（`docs/02` §D4）——不因解析不到时间就丢内容；
//! 3. **解析出 0 条 → `cursor: None`**（`docs/02` §B4）——到底信号只有这一个来源。

use serde_json::Value;

use xspider_core::error::{XError, XResult};
use xspider_core::paging::{Page, SeenIds};

use crate::calls::RawCall;
use crate::post::{parse_post, unwrap_retweet, unwrap_visibility, Post};
use crate::user::{parse_user_in_result, User};
use crate::FetchClient;

/// 上游默认页大小（`calls.rs` 里组装请求时用同一个常量，这里只读）。
use crate::calls::{DEFAULT_COUNT, DEFAULT_FOLLOWING_COUNT};

// ---------------------------------------------------------------------------
// 遍历结果与统计
// ---------------------------------------------------------------------------

/// 遍历的统计量。**测试会断言它们**——例如"广告被过滤干净"这一条，
/// 只看 `posts` 是看不出来的（没过滤掉的时候 posts 里会多一条）。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Collected {
    pub posts: Vec<Post>,
    /// 看起来像推文的候选条目数（不含广告）。
    pub candidates: usize,
    /// 其中解析失败的条数。
    pub unparseable: usize,
    /// 被识别为广告而丢弃的条数（`docs/02` §C1）。
    pub dropped_promoted: usize,
    /// 因为 `include_retweets=false` 而丢弃的转推条数（`docs/02` §C3）。
    pub dropped_retweet: usize,
    /// 因为 `require_media=true` 而丢弃的无媒体条数。
    pub dropped_no_media: usize,
    /// 因为 id 已经在前面出现过而丢弃的条数（重复转推）。
    pub dropped_duplicate: usize,
}

impl Collected {
    /// 「候选全废」= 很可能 X 改版了。正常的过滤（无媒体/转推/重复）**不算**。
    pub(crate) fn looks_like_a_breaking_change(&self) -> bool {
        self.candidates > 0 && self.posts.is_empty() && self.unparseable == self.candidates
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TimelineOptions {
    /// 是否要求带媒体（上游 `UserTweets` 恒为 true）。
    pub require_media: bool,
    /// 是否展开转推（true 时把原推提上来并填 `retweeted_by`）。
    pub include_retweets: bool,
}

impl Default for TimelineOptions {
    fn default() -> Self {
        Self {
            require_media: true,
            include_retweets: false,
        }
    }
}

// ---------------------------------------------------------------------------
// 指令遍历
// ---------------------------------------------------------------------------

/// 按候选路径取 `instructions`（首个命中的赢）。
pub(crate) fn instructions_at<'a>(root: &'a Value, paths: &[&[&str]]) -> Option<&'a Vec<Value>> {
    for path in paths {
        let mut node = root;
        let mut ok = true;
        for key in *path {
            match node.get(*key) {
                Some(next) => node = next,
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            if let Some(arr) = node.as_array() {
                return Some(arr);
            }
        }
    }
    None
}

/// 按**指令顺序**取出所有 entry。
///
/// 认两种指令：
/// - `TimelineAddEntries` → `entries[]`（时间线主体）；
/// - `TimelinePinEntry` → 单个 `entry`（**置顶推文**）。
///
/// 第二条是实测补上的：真实响应里置顶推文是**单独一条指令**，
/// 只认 `TimelineAddEntries` 会把置顶推文整条丢掉（而它可能带媒体——
/// 实测 `user_tweets` 首页里唯一带媒体的就是置顶那条）。
/// 上游 TS 与 Swift 移植都没有处理这个指令，所以这里与它们**行为不同**，
/// 差异是"多一条真实存在的推文"，不是少。
pub(crate) fn all_entries(instructions: &[Value]) -> Vec<&Value> {
    let mut out = Vec::new();
    for instruction in instructions {
        match instruction.get("type").and_then(Value::as_str) {
            Some("TimelineAddEntries") => {
                if let Some(entries) = instruction.get("entries").and_then(Value::as_array) {
                    out.extend(entries.iter());
                }
            }
            Some("TimelinePinEntry") => {
                if let Some(entry) = instruction.get("entry") {
                    out.push(entry);
                }
            }
            _ => {}
        }
    }
    out
}

/// 取 `Bottom` 游标。**没有 `Top` 游标这回事**（上游两个实现都只认 Bottom）。
pub(crate) fn bottom_cursor(instructions: &[Value]) -> Option<String> {
    all_entries(instructions).into_iter().find_map(|entry| {
        let content = entry.get("content")?;
        if content.get("cursorType").and_then(Value::as_str) == Some("Bottom") {
            content
                .get("value")
                .and_then(Value::as_str)
                .map(str::to_string)
        } else {
            None
        }
    })
}

/// 广告条目：entryId 以 `promoted` 开头（`docs/02` §C1 的第一个入口）。
fn is_promoted_entry_id(entry_id: &str) -> bool {
    entry_id.starts_with("promoted")
}

/// 广告条目：`itemContent.promotedMetadata` 非空（`docs/02` §C1 的第二、三个入口）。
fn is_promoted_item_content(item_content: &Value) -> bool {
    fn non_empty(value: Option<&Value>) -> bool {
        value.is_some_and(|v| match v {
            Value::Object(o) => !o.is_empty(),
            Value::Array(a) => !a.is_empty(),
            Value::Null => false,
            _ => true,
        })
    }
    if non_empty(item_content.get("promotedMetadata")) {
        return true;
    }
    non_empty(
        item_content
            .get("tweet_results")
            .and_then(|t| t.get("result"))
            .map(unwrap_visibility)
            .and_then(|r| r.get("promotedMetadata")),
    )
}

/// 把一条 `result` 收进结果集：**先记 id，再过滤**（`docs/02` §D1）。
fn push_result(out: &mut Collected, seen: &mut SeenIds, result: &Value, opts: TimelineOptions) {
    let result = unwrap_visibility(result);

    let is_retweet = result
        .get("legacy")
        .and_then(|l| l.get("retweeted_status_result"))
        .is_some();
    let mut retweeted_by = None;
    let parsed = if is_retweet {
        if !opts.include_retweets {
            out.dropped_retweet += 1;
            return;
        }
        match unwrap_retweet(result) {
            Some((post, by)) => {
                retweeted_by = by;
                Some(post)
            }
            None => None,
        }
    } else {
        parse_post(result)
    };

    let Some(mut post) = parsed else {
        out.candidates += 1;
        out.unparseable += 1;
        return;
    };

    // 1) 去重：**必须在筛选之前**，否则不同来源的重复项会漏网
    if !seen.insert(&post.id) {
        out.dropped_duplicate += 1;
        return;
    }
    out.candidates += 1;

    // 2) 筛选
    if opts.require_media && !post.has_media() {
        out.dropped_no_media += 1;
        return;
    }
    post.retweeted_by = retweeted_by;
    out.posts.push(post);
}

/// module 型遍历：`UserMedia` / `SearchTimeline` 用。
///
/// 与上游的差别（刻意）：上游只取**第一个** `TimelineTimelineModule`，
/// 这里遍历**全部** module。理由是"只取第一个"会在响应里出现多个 module 时
/// **静默丢条目**——那是数据丢失，比多写几行循环严重得多。
pub(crate) fn collect_from_modules(instructions: &[Value], opts: TimelineOptions) -> Collected {
    let mut out = Collected::default();
    let mut seen = SeenIds::new();

    for entry in all_entries(instructions) {
        if let Some(entry_id) = entry.get("entryId").and_then(Value::as_str) {
            if is_promoted_entry_id(entry_id) {
                out.dropped_promoted += 1;
                continue;
            }
        }

        let content = entry.get("content");

        // (a) module 里的 items
        if content
            .and_then(|c| c.get("entryType"))
            .and_then(Value::as_str)
            == Some("TimelineTimelineModule")
        {
            for item in content
                .and_then(|c| c.get("items"))
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[])
            {
                collect_module_item(&mut out, &mut seen, item, opts);
            }
            continue;
        }

        // (b) 散落的 `tweet-*` 条目（有些响应不包 module）
        if let Some(entry_id) = entry.get("entryId").and_then(Value::as_str) {
            if entry_id.starts_with("tweet") {
                if let Some(item_content) = content.and_then(|c| c.get("itemContent")) {
                    if is_promoted_item_content(item_content) {
                        out.dropped_promoted += 1;
                    } else if let Some(result) = tweet_result_of(item_content) {
                        push_result(&mut out, &mut seen, result, opts);
                    }
                }
            }
        }
    }

    // (c) 兜底：`TimelineAddToModule`（老结构）
    if out.posts.is_empty() {
        for instruction in instructions {
            if instruction.get("type").and_then(Value::as_str) != Some("TimelineAddToModule") {
                continue;
            }
            for item in instruction
                .get("moduleItems")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[])
            {
                collect_module_item(&mut out, &mut seen, item, opts);
            }
        }
    }

    out
}

fn collect_module_item(
    out: &mut Collected,
    seen: &mut SeenIds,
    item: &Value,
    opts: TimelineOptions,
) {
    let item_content = match item.get("item").and_then(|i| i.get("itemContent")) {
        Some(ic) => ic,
        None => return,
    };
    if is_promoted_item_content(item_content) {
        out.dropped_promoted += 1;
        return;
    }
    if let Some(result) = tweet_result_of(item_content) {
        push_result(out, seen, result, opts);
    }
}

/// entry 型遍历：`UserTweets` / `HomeTimeline` / `TweetDetail` 的回复用。
pub(crate) fn collect_from_entries(instructions: &[Value], opts: TimelineOptions) -> Collected {
    let mut out = Collected::default();
    let mut seen = SeenIds::new();
    for entry in all_entries(instructions) {
        collect_entry(&mut out, &mut seen, entry, opts);
    }
    out
}

/// 单条 entry → 0..n 个推文结果。`TweetDetail` 的回复提取也复用它。
pub(crate) fn collect_entry(
    out: &mut Collected,
    seen: &mut SeenIds,
    entry: &Value,
    opts: TimelineOptions,
) {
    let Some(entry_id) = entry.get("entryId").and_then(Value::as_str) else {
        return;
    };
    if is_promoted_entry_id(entry_id) {
        out.dropped_promoted += 1;
        return;
    }

    // 直接挂在 entry 上的推文
    if entry_id.starts_with("tweet") {
        if let Some(item_content) = entry.get("content").and_then(|c| c.get("itemContent")) {
            if is_promoted_item_content(item_content) {
                out.dropped_promoted += 1;
            } else if let Some(result) = tweet_result_of(item_content) {
                push_result(out, seen, result, opts);
            }
        }
        return;
    }

    // 自线程（`profile-conversation`）与评论串（`conversationthread`）都是一层 items
    let is_thread =
        entry_id.starts_with("profile-conversation") || entry_id.starts_with("conversationthread");
    if !is_thread {
        return;
    }
    for item in entry
        .get("content")
        .and_then(|c| c.get("items"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        let Some(item_content) = item.get("item").and_then(|i| i.get("itemContent")) else {
            continue;
        };
        if is_promoted_item_content(item_content) {
            out.dropped_promoted += 1;
            continue;
        }
        if let Some(result) = tweet_result_of(item_content) {
            push_result(out, seen, result, opts);
        }
    }
}

pub(crate) fn tweet_result_of(item_content: &Value) -> Option<&Value> {
    item_content.get("tweet_results")?.get("result")
}

/// 把遍历结果收敛成契约要求的 `Page`，并在"候选全废"时报 `parse`。
pub(crate) fn finish_page(
    endpoint: &'static str,
    collected: Collected,
    instructions: &[Value],
) -> XResult<Page<Post>> {
    if collected.looks_like_a_breaking_change() {
        return Err(XError::Parse {
            // 错误里带上下文，线上才定位得到（docs/05 §6）
            context: format!("{endpoint}.items"),
            message: format!(
                "{} 条候选推文全部解析失败（0 条成功）。这通常意味着 X 改了响应结构，\
                 请把原始响应存成 fixtures/{endpoint}/field_changed_<日期>.json 再修解析。",
                collected.unparseable
            ),
            endpoint: Some(endpoint.to_string()),
        });
    }
    // `docs/02` §B4：解析出 0 条时游标必须是 null（到底信号）。
    // 注意"被筛选清空"与"服务端没内容"在这里都归为 `.empty()`——
    // 这正是 §D5 要求的：到底判据只看服务端原始条数，不看筛选后的条数。
    if collected.posts.is_empty() && collected.candidates == 0 {
        return Ok(Page::empty());
    }
    Ok(Page::new(collected.posts, bottom_cursor(instructions)))
}

// ---------------------------------------------------------------------------
// 端点实现
// ---------------------------------------------------------------------------

impl FetchClient {
    /// `fetch.user_medias`：媒体时间线（上游 `getUserMedias`）。
    pub async fn user_medias(
        &self,
        user_id: &str,
        cursor: Option<&str>,
        count: Option<u64>,
        cancel: &xspider_core::cancel::CancelToken,
    ) -> XResult<Page<Post>> {
        let call = RawCall::UserMedias {
            user_id,
            cursor,
            count: count.or(Some(DEFAULT_COUNT)),
        };
        let body = self.call_json(call, cancel).await?;
        let instructions = instructions_at(
            &body,
            &[&[
                "data",
                "user",
                "result",
                "timeline_v2",
                "timeline",
                "instructions",
            ]],
        )
        .ok_or_else(|| missing_instructions(crate::calls::ENDPOINT_USER_MEDIAS, &body))?;

        // 媒体时间线的模块里本来就是媒体，不需要再按媒体筛一遍
        let collected = collect_from_modules(
            instructions,
            TimelineOptions {
                require_media: false,
                include_retweets: false,
            },
        );
        finish_page(crate::calls::ENDPOINT_USER_MEDIAS, collected, instructions)
    }

    /// `fetch.user_tweets`：推文时间线（上游 `getUserTweets`）。
    pub async fn user_tweets(
        &self,
        user_id: &str,
        cursor: Option<&str>,
        count: Option<u64>,
        require_media: Option<bool>,
        include_retweets: Option<bool>,
        cancel: &xspider_core::cancel::CancelToken,
    ) -> XResult<Page<Post>> {
        let call = RawCall::UserTweets {
            user_id,
            cursor,
            count: count.or(Some(DEFAULT_COUNT)),
        };
        let body = self.call_json(call, cancel).await?;
        let instructions = instructions_at(
            &body,
            &[&[
                "data",
                "user",
                "result",
                "timeline_v2",
                "timeline",
                "instructions",
            ]],
        )
        .ok_or_else(|| missing_instructions(crate::calls::ENDPOINT_USER_TWEETS, &body))?;

        // 上游 `getUserTweets` 恒为"要媒体、丢转推"；这里参数化，默认值与其一致
        let collected = collect_from_entries(
            instructions,
            TimelineOptions {
                require_media: require_media.unwrap_or(true),
                include_retweets: include_retweets.unwrap_or(false),
            },
        );
        finish_page(crate::calls::ENDPOINT_USER_TWEETS, collected, instructions)
    }

    /// `fetch.home_timeline`：主页时间线（`for_you` / `following`）。
    ///
    /// 内部固定 `require_media=false, include_retweets=true`——这是上游
    /// `getHomeTimeline` 的实测参数（主页时间线本来就混杂转推与纯文字）。
    pub async fn home_timeline(
        &self,
        mode: &str,
        cursor: Option<&str>,
        cancel: &xspider_core::cancel::CancelToken,
    ) -> XResult<Page<Post>> {
        let call = RawCall::HomeTimeline { mode, cursor };
        let body = self.call_json(call, cancel).await?;
        let instructions = instructions_at(
            &body,
            &[&["data", "home", "home_timeline_urt", "instructions"]],
        )
        .ok_or_else(|| missing_instructions(crate::calls::ENDPOINT_HOME_TIMELINE, &body))?;

        let collected = collect_from_entries(
            instructions,
            TimelineOptions {
                require_media: false,
                include_retweets: true,
            },
        );
        finish_page(
            crate::calls::ENDPOINT_HOME_TIMELINE,
            collected,
            instructions,
        )
    }

    /// `fetch.following`：关注列表。
    pub async fn following(
        &self,
        user_id: &str,
        cursor: Option<&str>,
        count: Option<u64>,
        cancel: &xspider_core::cancel::CancelToken,
    ) -> XResult<Page<User>> {
        let call = RawCall::Following {
            user_id,
            cursor,
            count: count.or(Some(DEFAULT_FOLLOWING_COUNT)),
        };
        let body = self.call_json(call, cancel).await?;

        // 两个真实出现过的路径，首个命中的赢
        let instructions = instructions_at(
            &body,
            &[
                &[
                    "data",
                    "user",
                    "result",
                    "timeline",
                    "timeline",
                    "instructions",
                ],
                &["data", "user", "result", "timeline", "instructions"],
            ],
        )
        .ok_or_else(|| missing_instructions(crate::calls::ENDPOINT_FOLLOWING, &body))?;

        let mut users = Vec::new();
        let mut seen = SeenIds::new();
        let mut candidates = 0usize;
        let mut unparseable = 0usize;
        for entry in all_entries(instructions) {
            let Some(entry_id) = entry.get("entryId").and_then(Value::as_str) else {
                continue;
            };
            if !entry_id.starts_with("user-") {
                continue;
            }
            let Some(result) = entry
                .get("content")
                .and_then(|c| c.get("itemContent"))
                .and_then(|ic| ic.get("user_results"))
                .and_then(|u| u.get("result"))
            else {
                continue;
            };
            candidates += 1;
            match parse_user_in_result(result) {
                Some(user) => {
                    // 与推文时间线同一条纪律：去重先于其它处理
                    if seen.insert(&user.id) {
                        users.push(user);
                    }
                }
                None => unparseable += 1,
            }
        }
        if candidates > 0 && users.is_empty() && unparseable == candidates {
            return Err(XError::Parse {
                context: format!("{}.users", crate::calls::ENDPOINT_FOLLOWING),
                message: format!("{unparseable} 条候选用户全部解析失败（X 可能改了用户结构）"),
                endpoint: Some(crate::calls::ENDPOINT_FOLLOWING.to_string()),
            });
        }

        Ok(Page::new(users, bottom_cursor(instructions)))
    }
}

pub(crate) fn missing_instructions(endpoint: &'static str, body: &Value) -> XError {
    let top_keys: Vec<String> = body
        .get("data")
        .and_then(|d| d.as_object())
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    XError::Parse {
        context: format!("{endpoint}.instructions"),
        message: format!(
            "响应里没有时间线 instructions（data 下的键：{top_keys:?}）。\
             这通常意味着 X 改了响应结构。"
        ),
        endpoint: Some(endpoint.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tweet_result_json(id: &str, with_media: bool) -> Value {
        let entities = if with_media {
            json!({ "media": [ { "type": "photo", "id_str": "m",
                                "media_url_https": "https://pbs.twimg.com/media/a.jpg" } ] })
        } else {
            json!({})
        };
        json!({
            "rest_id": id,
            "legacy": { "full_text": "t", "created_at": "Wed Sep 30 12:34:56 +0000 2009",
                        "favorite_count": 1, "entities": entities },
            "core": { "user_results": { "result": { "rest_id": "1", "legacy": {
                "screen_name": "u", "name": "U", "profile_image_url_https": "https://x/a.jpg" } } } }
        })
    }

    fn tweet_entry(entry_id: &str, post_id: &str, with_media: bool) -> Value {
        json!({
            "entryId": entry_id,
            "content": { "itemContent": { "tweet_results": { "result": tweet_result_json(post_id, with_media) } } }
        })
    }

    fn add_entries_instruction(entries: Vec<Value>) -> Vec<Value> {
        vec![json!({ "type": "TimelineAddEntries", "entries": entries })]
    }

    #[test]
    fn bottom_cursor_is_read_from_the_bottom_entry() {
        let instructions = add_entries_instruction(vec![
            tweet_entry("tweet-1", "1", true),
            json!({ "entryId": "cursor-top-1", "content": { "cursorType": "Top", "value": "TOP" } }),
            json!({ "entryId": "cursor-bottom-1", "content": { "cursorType": "Bottom", "value": "BOT" } }),
        ]);
        assert_eq!(bottom_cursor(&instructions).as_deref(), Some("BOT"));
        assert!(bottom_cursor(&[]).is_none());
        // 只有 Top 游标 → 没有 Bottom → None（**没有 Top 游标这回事**）
        let only_top = add_entries_instruction(vec![
            json!({ "content": { "cursorType": "Top", "value": "TOP" } }),
        ]);
        assert!(bottom_cursor(&only_top).is_none(), "只认 Bottom 游标");
    }

    #[test]
    fn entries_walker_filters_promoted_entries() {
        let instructions = add_entries_instruction(vec![
            tweet_entry("promoted-1", "ad", true),
            tweet_entry("tweet-1", "1", true),
        ]);
        let out = collect_from_entries(&instructions, TimelineOptions::default());
        assert_eq!(out.posts.len(), 1, "广告必须被过滤");
        assert_eq!(out.posts[0].id, "1");
        assert_eq!(out.dropped_promoted, 1, "要能报出过滤了几条广告");
    }

    #[test]
    fn entries_walker_filters_promoted_item_content() {
        let mut entry = tweet_entry("tweet-2", "2", true);
        entry["content"]["itemContent"]["promotedMetadata"] = json!({ "advertiser_results": {} });
        let instructions = add_entries_instruction(vec![entry, tweet_entry("tweet-3", "3", true)]);
        let out = collect_from_entries(&instructions, TimelineOptions::default());
        assert_eq!(out.posts.len(), 1);
        assert_eq!(out.posts[0].id, "3");
        assert_eq!(out.dropped_promoted, 1);
    }

    #[test]
    fn empty_promoted_metadata_uses_a_non_empty() {
        let mut entry = tweet_entry("tweet-4", "4", true);
        entry["content"]["itemContent"]["promotedMetadata"] = json!({});
        let instructions = add_entries_instruction(vec![entry]);
        let out = collect_from_entries(&instructions, TimelineOptions::default());
        assert_eq!(out.posts.len(), 1, "空的 promotedMetadata 不算广告");
        assert_eq!(out.dropped_promoted, 0);
    }

    #[test]
    fn modules_walker_reads_all_modules_not_just_the_first() {
        // 上游只取第一个 module；只取第一个会在多 module 响应里静默丢条目
        let instructions = add_entries_instruction(vec![
            json!({ "entryId": "module-1", "content": {
                "entryType": "TimelineTimelineModule",
                "items": [ { "item": { "itemContent": { "tweet_results": { "result": tweet_result_json("1", true) } } } } ]
            }}),
            json!({ "entryId": "module-2", "content": {
                "entryType": "TimelineTimelineModule",
                "items": [ { "item": { "itemContent": { "tweet_results": { "result": tweet_result_json("2", true) } } } } ]
            }}),
        ]);
        let out = collect_from_modules(&instructions, TimelineOptions::default());
        assert_eq!(out.posts.len(), 2, "两个 module 的条目都要拿到");
    }

    #[test]
    fn modules_walker_falls_back_to_scattered_tweet_entries() {
        let instructions = add_entries_instruction(vec![tweet_entry("tweet-9", "9", true)]);
        let out = collect_from_modules(&instructions, TimelineOptions::default());
        assert_eq!(out.posts.len(), 1);
        assert_eq!(out.posts[0].id, "9");
    }

    /// 实测：置顶推文在**单独的 `TimelinePinEntry` 指令**里。
    /// 只认 `TimelineAddEntries` 会把它整条丢掉——而它可能正是本页唯一带媒体的推文。
    #[test]
    fn pinned_entry_is_collected_too() {
        let instructions = vec![
            json!({ "type": "TimelineClearCache" }),
            json!({ "type": "TimelinePinEntry", "entry": tweet_entry("tweet-pinned", "pinned", true) }),
            json!({ "type": "TimelineAddEntries", "entries": [tweet_entry("tweet-1", "1", false)] }),
        ];
        let out = collect_from_entries(&instructions, TimelineOptions::default());
        assert_eq!(out.posts.len(), 1, "严格模式下应当留下置顶那条（带媒体）");
        assert_eq!(out.posts[0].id, "pinned");
        assert_eq!(out.dropped_no_media, 1, "另一条无媒体应被筛掉");

        // 宽松模式下两条都在，且顺序是"指令顺序"（置顶在前）
        let loose = collect_from_entries(
            &instructions,
            TimelineOptions {
                require_media: false,
                include_retweets: false,
            },
        );
        let ids: Vec<&str> = loose.posts.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["pinned", "1"]);
    }

    /// docs/02 §D5：**被客户端筛空 ≠ 到底**。
    /// 整页都是纯文字时，严格模式会得到空 items，但**游标必须留着**，
    /// 否则爬虫会以为"没有更多了"而提前停下（那是最容易漏掉的一类终止 bug）。
    #[test]
    fn a_page_emptied_by_filtering_still_carries_its_cursor() {
        let instructions = add_entries_instruction(vec![
            tweet_entry("tweet-1", "1", false),
            tweet_entry("tweet-2", "2", false),
            json!({ "entryId": "cursor-bottom-1", "content": { "cursorType": "Bottom", "value": "MORE" } }),
        ]);
        let collected = collect_from_entries(&instructions, TimelineOptions::default());
        assert!(collected.posts.is_empty(), "两条都无媒体 → 严格模式全筛掉");
        assert_eq!(
            collected.candidates, 2,
            "但候选数是 2（服务端确实给了内容）"
        );
        let page = finish_page("user_tweets", collected, &instructions).unwrap();
        assert!(
            !page.end,
            "被筛选清空不是到底：end 必须为 false（docs/02 §D5）"
        );
        assert_eq!(
            page.cursor.as_deref(),
            Some("MORE"),
            "游标必须保留，否则调用方没法继续翻页"
        );
    }

    #[test]
    fn dedup_drops_repeated_ids_in_the_same_page() {
        let instructions = add_entries_instruction(vec![
            tweet_entry("tweet-1", "same", true),
            tweet_entry("tweet-2", "same", true),
            tweet_entry("tweet-3", "other", true),
        ]);
        let out = collect_from_entries(&instructions, TimelineOptions::default());
        assert_eq!(out.posts.len(), 2);
        assert_eq!(out.dropped_duplicate, 1);
    }

    #[test]
    fn retweets_are_dropped_by_default_and_flattened_when_asked() {
        let mut retweet = tweet_entry("tweet-1", "outer", true);
        retweet["content"]["itemContent"]["tweet_results"]["result"]["legacy"]
            ["retweeted_status_result"] = json!({ "result": tweet_result_json("inner", true) });
        let instructions = add_entries_instruction(vec![retweet]);

        let dropped = collect_from_entries(&instructions, TimelineOptions::default());
        assert!(dropped.posts.is_empty());
        assert_eq!(dropped.dropped_retweet, 1);

        let kept = collect_from_entries(
            &instructions,
            TimelineOptions {
                require_media: false,
                include_retweets: true,
            },
        );
        assert_eq!(kept.posts.len(), 1);
        assert_eq!(kept.posts[0].id, "inner", "展开后应该是原推");
        assert!(kept.posts[0].retweeted_by.is_some(), "要记下是谁转的");
    }

    #[test]
    fn require_media_false_keeps_text_only_posts() {
        let instructions =
            add_entries_instruction(vec![tweet_entry("tweet-1", "text-only", false)]);
        let strict = collect_from_entries(&instructions, TimelineOptions::default());
        assert!(strict.posts.is_empty());
        assert_eq!(strict.dropped_no_media, 1);

        let loose = collect_from_entries(
            &instructions,
            TimelineOptions {
                require_media: false,
                include_retweets: false,
            },
        );
        assert_eq!(loose.posts.len(), 1, "纯文字推文在宽松模式下必须保留");
    }

    #[test]
    fn all_candidates_failing_is_a_parse_error() {
        let instructions = add_entries_instruction(vec![
            tweet_entry("tweet-1", "1", true),
            tweet_entry("tweet-2", "2", true),
        ]);
        let mut collected = collect_from_entries(&instructions, TimelineOptions::default());
        collected.posts.clear();
        collected.unparseable = collected.candidates;
        assert!(collected.looks_like_a_breaking_change());
        let err = finish_page("user_tweets", collected, &instructions).unwrap_err();
        assert_eq!(err.code(), xspider_core::error::ErrorCode::Parse);
        assert!(err.to_string().contains("全部解析失败"), "{err}");
        // 错误里要给出"存成哪个文件"的指引（docs/04 §6）
        assert!(err.to_string().contains("field_changed_"), "{err}");
    }

    #[test]
    fn normal_filtering_is_not_mistaken_for_a_breaking_change() {
        let instructions =
            add_entries_instruction(vec![tweet_entry("tweet-1", "text-only", false)]);
        let collected = collect_from_entries(&instructions, TimelineOptions::default());
        assert!(collected.posts.is_empty());
        assert!(!collected.looks_like_a_breaking_change());
        let page = finish_page("user_tweets", collected, &instructions).unwrap();
        assert!(page.end);
    }

    #[test]
    fn empty_page_yields_no_cursor() {
        // docs/02 §B4：解析出 0 条 → cursor null（到底信号）
        let instructions = add_entries_instruction(vec![json!({
            "entryId": "cursor-bottom-1",
            "content": { "cursorType": "Bottom", "value": "STILL-HERE" }
        })]);
        let collected = collect_from_entries(&instructions, TimelineOptions::default());
        assert_eq!(collected.candidates, 0);
        let page = finish_page("user_tweets", collected, &instructions).unwrap();
        assert!(page.end, "没有候选条目时必须是 end");
        assert!(
            page.cursor.is_none(),
            "空页不能回吐游标，否则会无限重抓同一页"
        );
    }

    #[test]
    fn instructions_lookup_tries_paths_in_order() {
        let body = json!({ "data": { "user": { "result": { "timeline": { "instructions": [ { "type": "X" } ] } } } } });
        let found = instructions_at(
            &body,
            &[
                &[
                    "data",
                    "user",
                    "result",
                    "timeline_v2",
                    "timeline",
                    "instructions",
                ],
                &["data", "user", "result", "timeline", "instructions"],
            ],
        );
        assert!(found.is_some(), "第二个路径必须被兜住");
        assert!(instructions_at(&body, &[&["data", "nope"]]).is_none());
    }

    #[test]
    fn missing_instructions_error_lists_top_level_keys() {
        let body = json!({ "data": { "wat": 1 } });
        let err = missing_instructions("user_tweets", &body);
        assert_eq!(err.code(), xspider_core::error::ErrorCode::Parse);
        assert!(err.to_string().contains("wat"), "{err}");
    }
}
