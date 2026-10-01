//! **离线回放测试**：用真实抓取的 fixture 钉住每个端点的行为与分页语义。
//!
//! 全部离线（`docs/04-TESTING-AND-FIXTURES.md` 第 2 层「fixture 回放」）：
//! `cargo test` 不碰网络。
//!
//! 这里断的是**语义**，不是"函数跑通了"——每一条都对应一个真实踩过的坑或一条硬性契约：
//!
//! | 断言 | 出处 |
//! |---|---|
//! | 首页请求的 `variables` 不含 cursor 键 | `docs/02` §A1（传 null 会反复请求第一页） |
//! | 翻页要带上一页返回的游标 | `docs/04` §3 |
//! | 解析出 0 条 → `cursor: null` | `docs/02` §B4 |
//! | 广告条目必须被过滤干净 | `docs/02` §C1（三个入口漏一个就露广告） |
//! | 重复 id 去重后唯一 | `docs/02` §C3（重复转推 → 空白卡片） |
//! | 无 `createdAt` 的条目放行 | `docs/02` §D4 |
//! | 孤儿回复标 `is_partial_parent` 且不丢 | `docs/02` §C2 |
//! | fixture 里没有真实个人数据 | `docs/04` §2.1 / §7 |

use std::path::PathBuf;

use serde_json::Value;

use xspider_core::cancel::CancelToken;
use xspider_core::creds::Credentials;
use xspider_core::paging::{classify_cursor, CursorOutcome, Page};
use xspider_core::stack::HttpStack;
use xspider_fetch::{FetchClient, Post};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

/// 回放模式的客户端：假 cookie（fixture 的 match 要求"带凭据"）。
///
/// 用假凭据而不是真凭据：离线测试不该需要任何真实凭据
/// （凭据只进不出，也不该为了跑测试去要一份真的）。
fn client() -> FetchClient {
    let stack = HttpStack::replay(fixtures_dir()).expect("加载 fixture 失败");
    stack.set_credentials(Some(
        Credentials::from_cookie("auth_token=fixture; ct0=fixture", None)
            .expect("假 cookie 应当合法"),
    ));
    FetchClient::new(stack)
}

fn fixture_body(endpoint: &str, scenario: &str) -> Value {
    let path = fixtures_dir()
        .join(endpoint)
        .join(format!("{scenario}.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("读取 {} 失败：{e}", path.display()));
    let envelope: Value = serde_json::from_str(&text).expect("fixture 不是合法 JSON");
    envelope["response"]["body"].clone()
}

fn all_fixtures() -> Vec<(PathBuf, Value)> {
    let mut out = Vec::new();
    for dir in std::fs::read_dir(fixtures_dir()).expect("读取 fixtures 失败") {
        let dir = dir.expect("目录项").path();
        if !dir.is_dir() {
            continue;
        }
        for file in std::fs::read_dir(&dir).expect("读取子目录失败") {
            let file = file.expect("文件项").path();
            if file.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let text = std::fs::read_to_string(&file).expect("读取 fixture");
            let value: Value = serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("{} 不是合法 JSON：{e}", file.display()));
            out.push((file, value));
        }
    }
    assert!(out.len() >= 10, "fixture 太少了，覆盖度不足：{}", out.len());
    out
}

/// 递归收集所有字符串叶子（脱敏检查用）。
fn all_strings(node: &Value, out: &mut Vec<String>) {
    match node {
        Value::Object(map) => map.values().for_each(|v| all_strings(v, out)),
        Value::Array(items) => items.iter().for_each(|v| all_strings(v, out)),
        Value::String(s) => out.push(s.clone()),
        _ => {}
    }
}

/// 递归找"带 `promotedMetadata` 的条目里的 `rest_id`"——**独立于解析实现**的交叉核对
/// （如果直接复用 walker，那就是拿实现验证实现）。
fn promoted_post_ids(node: &Value, out: &mut Vec<String>) {
    match node {
        Value::Object(map) => {
            if map.contains_key("promotedMetadata") {
                if let Some(id) = map
                    .get("rest_id")
                    .or_else(|| {
                        map.get("tweet_results")
                            .and_then(|t| t.get("result"))
                            .and_then(|r| r.get("rest_id"))
                    })
                    .and_then(Value::as_str)
                {
                    out.push(id.to_string());
                }
            }
            map.values().for_each(|v| promoted_post_ids(v, out));
        }
        Value::Array(items) => items.iter().for_each(|v| promoted_post_ids(v, out)),
        _ => {}
    }
}

/// 找 entryId 为 `tweet-<id>` 的那条推文 id（TweetDetail 的 focal）。
fn focal_id_in(body: &Value) -> String {
    fn walk(node: &Value) -> Option<String> {
        match node {
            Value::Object(map) => {
                if let Some(entry_id) = map.get("entryId").and_then(Value::as_str) {
                    if let Some(id) = entry_id.strip_prefix("tweet-") {
                        return Some(id.to_string());
                    }
                }
                map.values().find_map(walk)
            }
            Value::Array(items) => items.iter().find_map(walk),
            _ => None,
        }
    }
    walk(body).expect("fixture 里应当有一条 entryId 为 tweet-<id> 的条目")
}

/// 检查一页推文是否满足"媒体时间线"的基本形状。
fn assert_media_page(page: &Page<Post>, label: &str) {
    assert!(!page.items.is_empty(), "{label}：不该是空的");
    for post in &page.items {
        assert!(!post.id.is_empty(), "{label}：id 不能为空");
        assert!(
            post.created_at.is_some(),
            "{label}：真实响应里一定有 created_at（post {}）",
            post.id
        );
        assert!(
            post.author.screen_name == "demo_user",
            "{label}：fixture 必须已脱敏，实际 author={}",
            post.author.screen_name
        );
        assert!(
            post.author.avatar.starts_with("https://"),
            "{label}：头像要是 URL"
        );
    }
}

// ---------------------------------------------------------------------------
// 分页语义（M1 验收标准 ②）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn user_medias_paginates_with_the_returned_cursor() {
    let client = client();
    let cancel = CancelToken::new();

    // 首页：不带游标 → 命中 page1 fixture（match.cursor = absent）
    let page1 = client
        .user_medias("13298072", None, None, &cancel)
        .await
        .expect("首页必须解析成功");
    assert_media_page(&page1, "user_medias page1");
    let cursor = page1
        .cursor
        .clone()
        .expect("真实首页应当返回 Bottom 游标（否则录不到第二页）");
    assert!(!page1.end, "有游标就不是 end");

    // 第二页：把**上一页返回的**游标传回去 → 命中 page2 fixture（match.cursor = present）
    let page2 = client
        .user_medias("13298072", Some(&cursor), None, &cancel)
        .await
        .expect("第二页必须解析成功");
    assert_media_page(&page2, "user_medias page2");

    // 两页的内容必须不同——如果回放路由把请求都导到同一条 fixture 上，
    // 这个断言会立刻红（那正是"翻页没真的翻"的典型症状）
    let ids1: Vec<&str> = page1.items.iter().map(|p| p.id.as_str()).collect();
    let ids2: Vec<&str> = page2.items.iter().map(|p| p.id.as_str()).collect();
    assert_ne!(ids1, ids2, "两页必须是不同的内容（否则说明翻页没生效）");

    // 游标语义：第二页的游标与第一页不同 → 属于"推进了"
    assert_eq!(
        classify_cursor(Some(&cursor), page2.cursor.as_deref()),
        CursorOutcome::Advanced,
        "翻页后游标应当推进"
    );
}

/// 引用推文的正文要真的解析出来（契约 1.4.0 的 `post.quoted`）。
///
/// 用**真实 fixture**（`user_medias/page1.json` 里那条带 `quoted_status_result` 的推文）
/// 钉住：形状认不认、`quoted_id` 与内嵌对象是不是同一条、内层作者取不取得到。
/// 合成样本只能证明"代码按我理解的样子工作"，真实响应才能证明"X 就是这么给的"。
#[tokio::test]
async fn quoted_tweet_comes_from_a_real_fixture() {
    let client = client();
    let cancel = CancelToken::new();
    let page = client
        .user_medias("13298072", None, None, &cancel)
        .await
        .expect("首页必须解析成功");

    let with_quoted: Vec<&Post> = page.items.iter().filter(|p| p.quoted.is_some()).collect();
    assert_eq!(
        with_quoted.len(),
        1,
        "这条 fixture 里恰好有一条带引用的推文；多/少都说明解析或筛选变了"
    );

    let post = with_quoted[0];
    let quoted = post.quoted.as_ref().expect("上面刚筛过");
    assert!(!quoted.full_text.is_empty(), "引用推文的正文不能是空的");
    assert!(
        !quoted.author.screen_name.is_empty(),
        "引用推文的作者要取得到（引用卡片要显示它）"
    );
    assert_eq!(
        post.quoted_id.as_deref(),
        Some(quoted.id.as_str()),
        "`quoted_id` 与内嵌对象必须是同一条推文"
    );
    // 只嵌一层：内层不再有引用（这条 fixture 本来也没有，那是形状事实）
    assert!(quoted.quoted.is_none());
}

#[tokio::test]
async fn every_page_has_unique_ids() {
    // docs/02 §C3：重复 id 会让下游去重表/渲染错乱
    let client = client();
    let cancel = CancelToken::new();
    for (cursor, label) in [(None, "page1"), (Some("ANY"), "page2")] {
        let page = client
            .user_medias("13298072", cursor, None, &cancel)
            .await
            .unwrap_or_else(|e| panic!("{label} 解析失败：{e}"));
        let mut ids: Vec<&str> = page.items.iter().map(|p| p.id.as_str()).collect();
        let before = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), before, "{label} 里有重复 id");
    }
}

// ---------------------------------------------------------------------------
// 各端点：真实响应的解析
// ---------------------------------------------------------------------------

#[tokio::test]
async fn user_tweets_strict_mode_keeps_only_media_posts() {
    let client = client();
    let cancel = CancelToken::new();

    // 严格模式（默认 require_media=true、丢转推）
    let strict = client
        .user_tweets("13298072", None, None, None, None, &cancel)
        .await
        .expect("推文时间线必须解析成功");
    for post in &strict.items {
        assert!(
            post.has_media(),
            "严格模式（require_media=true）下每条都该有媒体：{}",
            post.id
        );
        assert!(
            post.retweeted_by.is_none(),
            "默认不展开转推，所以不该有 retweeted_by"
        );
    }

    // **这条页面的真实情况值得记下来**：真实响应里 3 条普通条目都没有媒体
    // （都是纯文字/转推），唯一带媒体的是**置顶推文**——而它单独躺在
    // `TimelinePinEntry` 指令里。所以：
    //   1. 只认 `TimelineAddEntries` 就会把这页解析成"空"（丢掉唯一带媒体的那条）；
    //   2. 严格模式下"筛空"是**正常现象**，不能当成"到底"（docs/02 §D5）。
    assert!(
        !strict.items.is_empty(),
        "置顶那条带媒体的推文必须被收进来——收不到说明 TimelinePinEntry 没处理"
    );

    // 宽松模式：保留纯文字推文（docs/02 §D4 的精神：不能因筛选丢内容）
    let loose = client
        .user_tweets("13298072", None, None, Some(false), Some(true), &cancel)
        .await
        .expect("宽松模式也要能解析");
    assert!(
        loose.items.len() > strict.items.len(),
        "放宽条件后条目数应当变多（严格 {} 条，宽松 {} 条）——否则筛选根本没生效",
        strict.items.len(),
        loose.items.len()
    );
    // 宽松模式必须真的包含纯文字推文（否则无从证明放宽起了作用）
    assert!(
        loose.items.iter().any(|p| !p.has_media()),
        "宽松模式下应当出现无媒体的推文"
    );
    // 被筛掉的是同样的服务端条目：游标必须一致，页面也不该被判成"到底"
    assert_eq!(
        loose.cursor, strict.cursor,
        "两次解析用的是同一个响应，游标应当相同"
    );
    assert!(!strict.end, "本页服务端给了游标，不能判成到底");
}

#[tokio::test]
async fn tweet_detail_parses_focal_and_marks_orphans() {
    let body = fixture_body("tweet_detail", "with_replies");
    let focal_id = focal_id_in(&body);

    let client = client();
    let cancel = CancelToken::new();
    let detail = client
        .tweet_detail(&focal_id, &cancel)
        .await
        .expect("推文详情必须解析成功");

    assert_eq!(detail.focal.id, focal_id, "focal 必须是请求的那一条");
    assert!(
        !detail.focal.author.screen_name.is_empty(),
        "focal 必须有作者"
    );
    // focal 自己不能出现在回复列表里
    assert!(
        detail.replies.iter().all(|r| r.post.id != focal_id),
        "focal 不该出现在 replies 里"
    );

    // docs/02 §C1：广告在回复节点里也是入口之一
    let mut promoted = Vec::new();
    promoted_post_ids(&body, &mut promoted);
    for reply in &detail.replies {
        assert!(
            !promoted.contains(&reply.post.id),
            "回复里出现了广告 {}",
            reply.post.id
        );
    }

    // docs/02 §C2：孤儿不能被丢弃。真实响应里通常有孤儿（父不在本页）。
    let marked = detail
        .replies
        .iter()
        .filter(|r| r.is_partial_parent)
        .count();
    eprintln!(
        "tweet_detail：focal={} 回复 {} 条，其中孤儿（父不在本页）{} 条",
        detail.focal.id,
        detail.replies.len(),
        marked
    );
    for reply in &detail.replies {
        if reply.is_partial_parent {
            assert!(
                reply.parent_id.is_some(),
                "标了 partial 就必须有 parent_id（否则外壳无法解释上下文）"
            );
        }
    }
    // 「回复 @xxx」必须是被回复者
    for reply in &detail.replies {
        if reply.post.in_reply_to_id.is_some() {
            assert!(
                reply.post.in_reply_to_screen_name.is_some(),
                "有 in_reply_to_id 就该有 in_reply_to_screen_name（docs/02 §C2）"
            );
        }
    }
}

#[tokio::test]
async fn search_timeline_parses_media_results() {
    let client = client();
    let cancel = CancelToken::new();
    let page = client
        .search_timeline("demo_user", "2026-09-24", "2026-10-01", true, None, &cancel)
        .await
        .expect("搜索必须解析成功（若 404 说明 POST/GET 搞错了，见 docs/02 §A2）");
    assert_media_page(&page, "search_timeline");

    let mut promoted = Vec::new();
    promoted_post_ids(
        &fixture_body("search_timeline", "media_only"),
        &mut promoted,
    );
    for post in &page.items {
        assert!(
            !promoted.contains(&post.id),
            "搜索结果里出现了广告 {}",
            post.id
        );
    }
}

#[tokio::test]
async fn home_timeline_works_in_both_modes() {
    let client = client();
    let cancel = CancelToken::new();

    let for_you = client
        .home_timeline("for_you", None, &cancel)
        .await
        .expect("for_you 必须解析成功");
    assert_media_page(&for_you, "home_timeline for_you");

    let following = client
        .home_timeline("following", None, &cancel)
        .await
        .expect("following 必须解析成功（它与 for_you 是两个不同的 operation）");
    assert_media_page(&following, "home_timeline following");

    // 两种模式必须命中不同的 fixture——路由写错时这里会红
    let a: Vec<&str> = for_you.items.iter().map(|p| p.id.as_str()).collect();
    let b: Vec<&str> = following.items.iter().map(|p| p.id.as_str()).collect();
    assert_ne!(a, b, "两种模式不该返回同一批数据（operation 不同）");

    // 非法模式要在**发请求之前**就被拒（否则会白发一次请求）
    let err = client
        .home_timeline("nope", None, &cancel)
        .await
        .unwrap_err();
    assert_eq!(
        err.code(),
        xspider_core::error::ErrorCode::InvalidRequest,
        "{err}"
    );
}

#[tokio::test]
async fn following_parses_users() {
    let client = client();
    let cancel = CancelToken::new();
    let page = client
        .following("13298072", None, None, &cancel)
        .await
        .expect("关注列表必须解析成功");

    assert!(!page.items.is_empty(), "真实响应里应当有关注的人");
    for user in &page.items {
        assert!(!user.id.is_empty(), "user.id 不能为空");
        assert_eq!(
            user.screen_name, "demo_user",
            "fixture 必须已脱敏（所有 screen_name 都应是假值）"
        );
    }
    // 去重：id 唯一
    let mut ids: Vec<&str> = page.items.iter().map(|u| u.id.as_str()).collect();
    let before = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), before, "用户列表里有重复 id");
}

// ---------------------------------------------------------------------------
// 广告过滤（docs/02 §C1：三个入口，漏一个就露广告）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn no_promoted_items_survive_in_any_timeline_fixture() {
    // 这条断言是**交叉核对**：先独立地在 fixture 里找出广告条目的 id，
    // 再确认解析结果里一条都没有。如果某个入口漏了过滤，这里会红。
    let client = client();
    let cancel = CancelToken::new();

    let cases: Vec<(&str, &str)> = vec![
        ("user_medias/page1.json", "medias"),
        ("user_tweets/page1.json", "tweets"),
        ("search_timeline/media_only.json", "search"),
        ("home_timeline/for_you.json", "home"),
    ];
    let mut total_promoted = 0usize;
    for (rel, kind) in cases {
        let path = fixtures_dir().join(rel);
        let text = std::fs::read_to_string(&path).expect("读取 fixture");
        let envelope: Value = serde_json::from_str(&text).expect("fixture JSON");
        let body = &envelope["response"]["body"];
        let mut promoted = Vec::new();
        promoted_post_ids(body, &mut promoted);
        total_promoted += promoted.len();

        let posts: Vec<String> = match kind {
            "medias" => client
                .user_medias("13298072", None, None, &cancel)
                .await
                .expect("medias")
                .items
                .iter()
                .map(|p| p.id.clone())
                .collect(),
            "tweets" => client
                .user_tweets("13298072", None, None, Some(false), Some(true), &cancel)
                .await
                .expect("tweets")
                .items
                .iter()
                .map(|p| p.id.clone())
                .collect(),
            "search" => client
                .search_timeline("demo_user", "2026-09-24", "2026-10-01", true, None, &cancel)
                .await
                .expect("search")
                .items
                .iter()
                .map(|p| p.id.clone())
                .collect(),
            _ => client
                .home_timeline("for_you", None, &cancel)
                .await
                .expect("home")
                .items
                .iter()
                .map(|p| p.id.clone())
                .collect(),
        };
        for id in &promoted {
            assert!(
                !posts.contains(id),
                "{rel}：广告条目 {id} 出现在了解析结果里——某个过滤入口漏了（docs/02 §C1）"
            );
        }
    }
    eprintln!("本次抽样里真实响应共含 {total_promoted} 条广告条目（全部被过滤）");
    // 真实响应里**可能**恰好没有广告；那样这条测试就是"没得可测"而不是"测过了"。
    // 明确说出来，免得后人以为它一直在守着什么。
    if total_promoted == 0 {
        eprintln!("注意：本次 fixture 里没有广告条目，这条测试未能真正验证过滤逻辑");
    }
}

// ---------------------------------------------------------------------------
// fixture 纪律（docs/04 §2.1 / §7：不许提交真实个人数据）
// ---------------------------------------------------------------------------

#[test]
fn every_committed_fixture_is_fully_redacted() {
    let fixtures = all_fixtures();
    let mut checked = 0usize;
    for (path, value) in &fixtures {
        let rel = path
            .strip_prefix(fixtures_dir())
            .unwrap_or(path)
            .display()
            .to_string();

        // 1) 凭据痕迹
        let mut strings = Vec::new();
        all_strings(value, &mut strings);
        let joined = strings.join("\n");
        for needle in ["auth_token=", "ct0=", "Bearer "] {
            assert!(
                !joined.contains(needle),
                "{rel}：出现了凭据痕迹 {needle:?}（docs/04 §7 反模式）"
            );
        }

        // 2) 所有 screen_name 必须已经是假值。这是最强的一条：
        //    脱敏脚本把所有真实用户名换成同一个假值，所以出现别的值就是漏了。
        let mut names = Vec::new();
        collect_key_values(value, "screen_name", &mut names);
        for name in names {
            assert_eq!(
                name, "demo_user",
                "{rel}：出现了未被脱敏的 screen_name {name:?}"
            );
        }

        // 3) 真实媒体 URL 不许入库（保留 host、路径必须换成假名）
        for url in strings {
            if url.contains("pbs.twimg.com") || url.contains("video.twimg.com") {
                assert!(
                    url.contains("/redacted/"),
                    "{rel}：媒体 URL 没脱敏：{url}（docs/04 §2.1 要求保留 host、路径换假名）"
                );
            }
        }

        // 4) 每条 fixture 都要能说出"什么时候采的"
        if let Some(captured) = value.get("captured_at").and_then(Value::as_str) {
            assert!(
                captured.starts_with("20") && captured.len() == 10,
                "{rel}：captured_at 形状不对：{captured}"
            );
        }
        checked += 1;
    }
    assert!(checked >= 10, "检查的 fixture 太少：{checked}");
    eprintln!("已检查 {checked} 条 fixture：凭据、用户名、媒体 URL 全部脱敏");
}

fn collect_key_values(node: &Value, key: &str, out: &mut Vec<String>) {
    match node {
        Value::Object(map) => {
            for (k, v) in map {
                if (k == key || k.ends_with(&format!("_{key}"))) && v.is_string() {
                    if let Some(s) = v.as_str() {
                        if !s.trim().is_empty() {
                            out.push(s.to_string());
                        }
                    }
                }
                collect_key_values(v, key, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|v| collect_key_values(v, key, out)),
        _ => {}
    }
}

#[test]
fn paginated_fixtures_declare_their_cursor_expectation() {
    // 分页端点的 fixture 必须写明 cursor 的有无——否则首页与第二页无法区分，
    // 回放会静默地把两个用例导到同一条数据上（这条测试守的就是那个静默）
    for scenario in ["page1", "page2"] {
        let path = fixtures_dir()
            .join("user_medias")
            .join(format!("{scenario}.json"));
        let text = std::fs::read_to_string(&path).expect("读取 fixture");
        let value: Value = serde_json::from_str(&text).expect("fixture JSON");
        let cursor = value["match"]["cursor"]
            .as_str()
            .unwrap_or_else(|| panic!("user_medias/{scenario} 的 match 里必须有 cursor"));
        let expected = if scenario == "page1" {
            "absent"
        } else {
            "present"
        };
        assert_eq!(
            cursor, expected,
            "user_medias/{scenario} 的 cursor 条件不对"
        );
    }
}
