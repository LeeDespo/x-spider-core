//! **live 录制**：把真实抓到的响应落盘，供脱敏后入库。
//!
//! ```bash
//! export XSPIDER_LIVE=1
//! export XSPIDER_COOKIE='auth_token=...; ct0=...'   # 绝不写进文件、绝不打日志
//! export XSPIDER_PROXY=http://127.0.0.1:17890       # 需要代理时（访问 x.com）
//! export XSPIDER_RECORD_SCREEN_NAME=tesla           # 录哪个账号（默认 tesla）
//!
//! cargo test -p xspider-fetch --test record_live --offline -- --ignored --nocapture
//! python3 script/redact_fixtures.py
//! ```
//!
//! 录出来的是**原始**响应，落在 `fixtures/<endpoint>/raw/`（已在 `.gitignore` 里）。
//! 脱敏之后才会变成入库的那个文件。
//!
//! 为什么分两步：`docs/04-TESTING-AND-FIXTURES.md` §2.1 的铁律是
//! **fixture 必须是真实抓到的响应**，而脱敏规则又必须可审计。
//! 把"抓"和"洗"分开，两件事各自可核对——直接手写一个"看起来对"的 JSON
//! 是这套测试最想避免的事（手写样本只能证明你按自己以为的格式解析正确）。
//!
//! 录制走的是**生产代码路径**（`FetchClient::raw` ⇒ 与端点方法同一个 `prepare()`），
//! 所以录下来的东西代表生产行为，不是"我以为的请求"。

use std::path::PathBuf;

use xspider_core::cancel::CancelToken;
use xspider_core::creds::Credentials;
use xspider_core::http::ProxyConfig;
use xspider_core::stack::HttpStack;
use xspider_core::transport::HttpResponse;
use xspider_core::xdate;
use xspider_fetch::search_query_id::{extract_main_bundle_url, extract_query_id};
use xspider_fetch::{FetchClient, RawCall};

fn live_env() -> Option<(String, ProxyConfig)> {
    if std::env::var("XSPIDER_LIVE").as_deref() != Ok("1") {
        eprintln!("跳过：需要 XSPIDER_LIVE=1（live 测试会消耗账号配额，默认不跑）");
        return None;
    }
    let cookie = match std::env::var("XSPIDER_COOKIE") {
        Ok(c) if !c.trim().is_empty() => c,
        _ => {
            eprintln!("跳过：需要 XSPIDER_COOKIE（由外壳/使用者注入，不要写进仓库）");
            return None;
        }
    };
    let proxy = match std::env::var("XSPIDER_PROXY") {
        Ok(url) if !url.trim().is_empty() => ProxyConfig::Manual(url.trim().to_string()),
        _ => ProxyConfig::Env,
    };
    Some((cookie, proxy))
}

fn target_screen_name() -> String {
    std::env::var("XSPIDER_RECORD_SCREEN_NAME").unwrap_or_else(|_| "tesla".to_string())
}

fn raw_dir(endpoint: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(endpoint)
        .join("raw");
    std::fs::create_dir_all(&dir).expect("应能创建 raw 目录");
    dir
}

/// 只保留有诊断价值的响应头：**不把整个头集合存进仓库**。
fn keep_headers(resp: &HttpResponse) -> serde_json::Map<String, serde_json::Value> {
    let mut out = serde_json::Map::new();
    for (name, value) in &resp.headers {
        let lower = name.to_ascii_lowercase();
        // x-rate-limit-* 是**有用**的：实测 X 会返回它，M1 可以据此做主动限流
        let interesting = lower == "content-type"
            || lower == "retry-after"
            || lower.starts_with("x-rate-limit")
            || lower == "x-transaction-id";
        if interesting {
            out.insert(lower, serde_json::Value::String(value.clone()));
        }
    }
    out
}

fn write_raw(endpoint: &str, scenario: &str, match_hint: serde_json::Value, resp: &HttpResponse) {
    let body = resp.body_text();
    let body_value: serde_json::Value =
        serde_json::from_str(&body).unwrap_or(serde_json::Value::String(body));

    let envelope = serde_json::json!({
        "endpoint": endpoint,
        "scenario": scenario,
        "captured_at": xdate::today_utc(),
        "match_hint": match_hint,
        "response": {
            "status": resp.status,
            "headers": keep_headers(resp),
            "body": body_value,
        }
    });
    let path = raw_dir(endpoint).join(format!("{scenario}.json"));
    std::fs::write(&path, serde_json::to_string_pretty(&envelope).unwrap())
        .unwrap_or_else(|e| panic!("写 {} 失败：{e}", path.display()));
    // 只报状态与体积，**不报响应内容**（可能含个人数据）
    eprintln!(
        "录制 {endpoint}/{scenario} → {}（HTTP {}，{} 字节）",
        path.display(),
        resp.status,
        resp.body.len()
    );
}

fn hint(method: &str, operation: &str, cursor: Option<&str>, auth: &str) -> serde_json::Value {
    let mut map = serde_json::json!({ "method": method, "operation": operation, "auth": auth });
    if let Some(cursor) = cursor {
        map["cursor"] = serde_json::json!(cursor);
    }
    map
}

fn build_client() -> Option<(FetchClient, CancelToken)> {
    let (cookie, proxy) = live_env()?;
    let stack = HttpStack::live(proxy).expect("构造 HTTP 栈失败");
    stack.set_credentials(Some(
        Credentials::from_cookie(cookie, None).expect("XSPIDER_COOKIE 里必须有 ct0"),
    ));
    Some((FetchClient::new(stack), CancelToken::new()))
}

// ---------------------------------------------------------------------------
// 用户端点（M0 的三条样本）
// ---------------------------------------------------------------------------

/// 一次把三类样本录完：正常 / 不存在 / 未授权。
///
/// 三类都是**真实响应**，且都不需要"故意打限流"：
/// - `normal`：真实存在的账号；
/// - `not_found`：真实请求一个不存在的用户名——X 会如实告诉我们它不存在；
/// - `unauthorized`：**不带 cookie** 发一次请求（既不消耗有效凭据，也不触发保护机制）。
///
/// 刻意**不**录制 429：拿到真实的 429 就得先把账号打进限流，
/// 那是拿使用者的账号配额去换一条测试数据，代价不对等（见 `fixtures/README.md`）。
#[tokio::test]
#[ignore = "需要真实网络与 cookie；用 XSPIDER_LIVE=1 显式开启"]
async fn record_user_by_screen_name() {
    let Some((client, cancel)) = build_client() else {
        return;
    };
    let target = target_screen_name();

    let resp = client
        .get_user_raw(&target, &cancel)
        .await
        .expect("正常样本必须成功（失败时先看是不是 cookie 过期了）");
    assert_eq!(resp.status, 200, "正常样本期望 HTTP 200");
    write_raw(
        "user_by_screen_name",
        "normal",
        hint("GET", "UserByScreenName", None, "valid"),
        &resp,
    );

    let missing = format!("xspider_no_such_user_{}", std::process::id());
    let resp = client
        .get_user_raw(&missing, &cancel)
        .await
        .expect("不存在的用户不该是传输层错误，而应是可解析的响应");
    write_raw(
        "user_by_screen_name",
        "not_found",
        hint("GET", "UserByScreenName", None, "valid"),
        &resp,
    );

    // 未授权：**不带凭据**再发一次。
    // 走 `get_user_anonymous`（不经凭据前置检查）而不是先 set_credentials(None)：
    // 后者会被我们自己的前置拦下来，那就录不到上游真实的 401/403 响应。
    let resp = client
        .get_user_anonymous(&target, &cancel)
        .await
        .expect("匿名请求应当拿到一个可解析的上游响应");
    write_raw(
        "user_by_screen_name",
        "unauthorized",
        hint("GET", "UserByScreenName", None, "none"),
        &resp,
    );
    assert!(
        resp.status == 401 || resp.status == 403,
        "匿名请求期望 401/403，实际 HTTP {} —— 如果 X 改了行为，这条断言就是报警器",
        resp.status
    );
}

// ---------------------------------------------------------------------------
// 时间线端点
// ---------------------------------------------------------------------------

/// 录制时间线类端点：媒体 / 推文 / 详情 / 搜索 / 主页 / 关注。
///
/// **会消耗 8 次左右的接口配额**（实测一次窗口 150 次，够用）。
#[tokio::test]
#[ignore = "需要真实网络与 cookie；用 XSPIDER_LIVE=1 显式开启"]
async fn record_timelines() {
    let Some((client, cancel)) = build_client() else {
        return;
    };
    let target = target_screen_name();

    // 先拿 user_id —— 时间线端点要的是数字 id
    let user = client
        .get_user(&target, &cancel)
        .await
        .expect("先取用户失败（cookie 过期？代理不通？）");
    eprintln!("目标账号 {target} → user_id={}", user.id);

    // 1) 媒体时间线：首页 + 第二页（第二页用来钉"翻页要带上一页的游标"）
    let page1 = client
        .raw(
            RawCall::UserMedias {
                user_id: &user.id,
                cursor: None,
                count: Some(20),
            },
            &cancel,
        )
        .await
        .expect("媒体时间线首页失败");
    write_raw(
        "user_medias",
        "page1",
        hint("GET", "UserMedia", Some("absent"), "valid"),
        &page1,
    );

    if let Some(cursor) = deep_bottom_cursor(&page1) {
        let page2 = client
            .raw(
                RawCall::UserMedias {
                    user_id: &user.id,
                    cursor: Some(&cursor),
                    count: Some(20),
                },
                &cancel,
            )
            .await
            .expect("媒体时间线第二页失败");
        write_raw(
            "user_medias",
            "page2",
            hint("GET", "UserMedia", Some("present"), "valid"),
            &page2,
        );
    } else {
        eprintln!("警告：首页没有 Bottom 游标，录不到第二页（该账号媒体太少？）");
    }

    // 2) 推文时间线
    let tweets = client
        .raw(
            RawCall::UserTweets {
                user_id: &user.id,
                cursor: None,
                count: Some(20),
            },
            &cancel,
        )
        .await
        .expect("推文时间线失败");
    write_raw(
        "user_tweets",
        "page1",
        hint("GET", "UserTweets", Some("absent"), "valid"),
        &tweets,
    );

    // 3) 推文详情：用推文时间线里第一条的 id（真实存在的推文）
    if let Some(tweet_id) = first_tweet_id(&tweets) {
        let detail = client
            .raw(RawCall::TweetDetail { id: &tweet_id }, &cancel)
            .await
            .expect("推文详情失败");
        write_raw(
            "tweet_detail",
            "with_replies",
            hint("GET", "TweetDetail", None, "valid"),
            &detail,
        );
    } else {
        eprintln!("警告：推文时间线里没有可用的推文 id，跳过 tweet_detail");
    }

    // 4) 关注列表
    let following = client
        .raw(
            RawCall::Following {
                user_id: &user.id,
                cursor: None,
                count: Some(100),
            },
            &cancel,
        )
        .await
        .expect("关注列表失败");
    write_raw(
        "following",
        "page1",
        hint("GET", "Following", Some("absent"), "valid"),
        &following,
    );

    // 5) 主页时间线（两种模式）
    for (mode, operation) in [
        ("for_you", "HomeTimeline"),
        ("following", "HomeLatestTimeline"),
    ] {
        let resp = client
            .raw(RawCall::HomeTimeline { mode, cursor: None }, &cancel)
            .await
            .unwrap_or_else(|e| panic!("主页时间线 {mode} 失败：{e}"));
        write_raw(
            "home_timeline",
            mode,
            hint("GET", operation, Some("absent"), "valid"),
            &resp,
        );
    }

    // 6) 搜索（最近 7 天，只要媒体）——顺带验证 POST + JSON body 这条路真的通
    let today = xdate::today_utc();
    let since = xdate::add_days(&today, -7).expect("日期计算");
    let search = client
        .raw(
            RawCall::SearchTimeline {
                screen_name: &target,
                since: &since,
                until: &today,
                media_only: true,
                cursor: None,
                count: Some(20),
            },
            &cancel,
        )
        .await
        .expect("搜索失败（若 404，先看 docs/02 §A2：GET 一定 404，POST 才对）");
    write_raw(
        "search_timeline",
        "media_only",
        hint("POST", "SearchTimeline", Some("absent"), "valid"),
        &search,
    );

    eprintln!(
        "\n录制完成。下一步：\n  python3 script/redact_fixtures.py\n\
         然后检查 fixtures/ 下的脱敏结果，raw/ 不要提交。"
    );
}

// ---------------------------------------------------------------------------
// 从原始响应里取录制脚本需要的位置（故意不复用内部实现，以免"录错地方"被掩盖）
// ---------------------------------------------------------------------------

fn parse(resp: &HttpResponse) -> Option<serde_json::Value> {
    serde_json::from_str(&resp.body_text()).ok()
}

fn deep_bottom_cursor(resp: &HttpResponse) -> Option<String> {
    fn walk(node: &serde_json::Value) -> Option<String> {
        match node {
            serde_json::Value::Object(map) => {
                if map.get("cursorType").and_then(|v| v.as_str()) == Some("Bottom") {
                    if let Some(value) = map.get("value").and_then(|v| v.as_str()) {
                        return Some(value.to_string());
                    }
                }
                map.values().find_map(walk)
            }
            serde_json::Value::Array(arr) => arr.iter().find_map(walk),
            _ => None,
        }
    }
    walk(&parse(resp)?)
}

fn first_tweet_id(resp: &HttpResponse) -> Option<String> {
    let body = parse(resp)?;
    let instructions = body
        .pointer("/data/user/result/timeline_v2/timeline/instructions")
        .and_then(|v| v.as_array())?;
    for instruction in instructions {
        let Some(entries) = instruction.get("entries").and_then(|v| v.as_array()) else {
            continue;
        };
        for entry in entries {
            let entry_id = entry.get("entryId").and_then(|v| v.as_str()).unwrap_or("");
            if !entry_id.starts_with("tweet") {
                continue;
            }
            if let Some(id) = entry
                .pointer("/content/itemContent/tweet_results/result/rest_id")
                .and_then(|v| v.as_str())
            {
                return Some(id.to_string());
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// 搜索 queryId 的来源（自愈逻辑的真实输入）
// ---------------------------------------------------------------------------

/// 录制搜索页与 main bundle 的关键片段，用来离线测 queryId 自愈。
///
/// 只存**匹配点附近**的一段，不存整个 bundle：那是一个随改版天天变的巨大文件，
/// 存进来既没法 review 也没必要。片段里必须包含那几个同名前缀的变体
/// （`BookmarkSearchTimeline` 等），否则"锚定 operationName"这条测试就失去意义。
#[tokio::test]
#[ignore = "需要真实网络（不需要 cookie）；用 XSPIDER_LIVE=1 显式开启"]
async fn record_search_query_id_source() {
    if std::env::var("XSPIDER_LIVE").as_deref() != Ok("1") {
        eprintln!("跳过：需要 XSPIDER_LIVE=1");
        return;
    }
    let proxy = match std::env::var("XSPIDER_PROXY") {
        Ok(url) if !url.trim().is_empty() => ProxyConfig::Manual(url.trim().to_string()),
        _ => ProxyConfig::Env,
    };
    let stack = HttpStack::live(proxy).expect("构造 HTTP 栈失败");
    // 搜索页**必须带凭据**（未登录会 307 到 onboarding 页，那里没有 bundle 引用）
    if let Ok(cookie) = std::env::var("XSPIDER_COOKIE") {
        stack.set_credentials(Credentials::from_cookie(cookie, None).ok());
    }
    let cancel = CancelToken::new();

    let page_url = "https://x.com/search?q=from%3Atwitter&src=typed_query&f=live";
    // 这一步**不需要 cookie**：搜索页与 bundle 都是公开资源
    let html = stack
        .fetch_text_with(page_url, true, xspider_core::RequestClass::Cdn, &cancel)
        .await
        .expect("抓搜索页失败");

    let bundle_url = extract_main_bundle_url(&html).expect("搜索页里找不到 main bundle URL");
    eprintln!("main bundle: {bundle_url}");

    let bundle = stack
        .fetch_text_with(&bundle_url, false, xspider_core::RequestClass::Cdn, &cancel)
        .await
        .expect("抓 bundle 失败");

    let query_id = extract_query_id(&bundle).expect("bundle 里找不到 SearchTimeline 的 queryId");

    // 所有 "queryId":"...",operationName:"..." 配对
    let pair_re = regex::Regex::new(r#"queryId:"([A-Za-z0-9_-]{20,24})",operationName:"([^"]+)""#)
        .expect("内置正则必须编译通过");
    let pairs: Vec<(String, String)> = pair_re
        .captures_iter(&bundle)
        .filter_map(|c| {
            Some((
                c.get(1)?.as_str().to_string(),
                c.get(2)?.as_str().to_string(),
            ))
        })
        .collect();

    // 一个**真实存在**的兄弟 operation：名字里含 SearchTimeline 但不是它。
    // 这正是"不锚定 operationName 就会匹配错"的现场证据。
    // 注意元组顺序是 (queryId, operationName)——第一版写反了，导致 sibling_excerpt 录成空，
    // 而"锚点必需"那条测试因此**静默地少断言了一半**（见 docs/05-WORKFLOW.md 的踩坑总索引）
    let (sibling_query_id, sibling_name) = pairs
        .iter()
        .find(|(_, name)| name.contains("SearchTimeline") && name != "SearchTimeline")
        .cloned()
        .unwrap_or_else(|| ("<无>".to_string(), "<无>".to_string()));

    // 未锚定的朴素做法会先命中哪个 queryId
    let first_query_id = pairs
        .first()
        .map(|(id, _)| id.clone())
        .unwrap_or_else(|| "<无>".to_string());

    let window = |needle: &str, before: usize, after: usize| -> Option<String> {
        let at = bundle.find(needle)?;
        let mut start = at.saturating_sub(before);
        while start > 0 && !bundle.is_char_boundary(start) {
            start -= 1;
        }
        let mut end = (at + needle.len() + after).min(bundle.len());
        while end < bundle.len() && !bundle.is_char_boundary(end) {
            end += 1;
        }
        Some(bundle[start..end].to_string())
    };

    let excerpt =
        window(&format!("queryId:\"{query_id}\""), 500, 300).expect("刚匹配到的位置必须能再找到");
    let sibling_excerpt = if sibling_query_id == "<无>" {
        String::new()
    } else {
        window(&format!("queryId:\"{sibling_query_id}\""), 120, 200).unwrap_or_default()
    };

    let fixture = serde_json::json!({
        "captured_at": xdate::today_utc(),
        "page_url": page_url,
        "bundle_url": bundle_url,
        "expected_query_id": query_id,
        "excerpt": excerpt,
        "sibling_operation": sibling_name,
        "sibling_query_id": sibling_query_id,
        "sibling_excerpt": sibling_excerpt,
        "unanchored_first_query_id": first_query_id,
        "note": "由 record_search_query_id_source 从真实 bundle 截取；只含匹配点附近的片段，不含个人数据"
    });

    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/search_timeline/query_id_source.json");
    std::fs::create_dir_all(path.parent().unwrap()).expect("创建目录失败");
    std::fs::write(&path, serde_json::to_string_pretty(&fixture).unwrap())
        .unwrap_or_else(|e| panic!("写 {} 失败：{e}", path.display()));
    eprintln!(
        "录制 queryId 来源 → {}（片段 {} 字符）",
        path.display(),
        fixture["excerpt"].as_str().map(str::len).unwrap_or(0)
    );
}
