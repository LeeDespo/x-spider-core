//! **live canary**：对真实 X 取数并断言解析成功（`docs/04-TESTING-AND-FIXTURES.md` §6）。
//!
//! ```bash
//! XSPIDER_LIVE=1 XSPIDER_COOKIE='...' XSPIDER_PROXY=http://127.0.0.1:17890 \
//!   cargo test -p xspider-fetch --test canary_live --offline -- --ignored --nocapture
//! ```
//!
//! 这是你的「X 又变了」报警器：
//! - 默认不跑（`#[ignore]` + `XSPIDER_LIVE=1` 双重门控）：它会消耗账号配额；
//! - **红了要能一眼定位**，所以报告里是
//!   「哪个端点 / 哪一步（请求还是解析）/ 哪个字段 / 原始响应片段」，
//!   而不是一句 `assertion failed`；
//! - 红了之后的第一件事不是改断言，而是把原始响应存进 fixtures 再修解析——
//!   **本文件在失败时会自动存**（`field_changed_<日期>.json`，未脱敏，入库前必须跑脱敏脚本）。
//!
//! 每个端点只取 1 页、不翻页：canary 的职责是"探测形态"，不是"取数据"。

use xspider_core::cancel::CancelToken;
use xspider_core::creds::Credentials;
use xspider_core::error::{ErrorCode, XResult};
use xspider_core::http::ProxyConfig;
use xspider_core::stack::HttpStack;
use xspider_core::xdate;
use xspider_fetch::{FetchClient, RawCall};

fn proxy_from_env() -> ProxyConfig {
    match std::env::var("XSPIDER_PROXY") {
        Ok(url) if !url.trim().is_empty() => ProxyConfig::Manual(url.trim().to_string()),
        _ => ProxyConfig::Env,
    }
}

fn credentials_from_env() -> Option<Credentials> {
    let cookie = std::env::var("XSPIDER_COOKIE").ok()?;
    if cookie.trim().is_empty() {
        return None;
    }
    Credentials::from_cookie(cookie, None).ok()
}

/// 探测结果累积器。红了的一次在最后统一报告，避免只看到第一条。
struct Report {
    checked: usize,
    failures: Vec<String>,
}

impl Report {
    fn new() -> Self {
        Self {
            checked: 0,
            failures: Vec::new(),
        }
    }

    fn pass(&mut self, endpoint: &str, detail: impl std::fmt::Display) {
        self.checked += 1;
        eprintln!("[ok] {endpoint}: {detail}");
    }

    fn fail(&mut self, endpoint: &str, stage: &str, detail: impl std::fmt::Display) {
        self.checked += 1;
        let line = format!("{endpoint}\n     阶段：{stage}\n     详情：{detail}");
        eprintln!("[!!] {line}");
        self.failures.push(line);
    }

    fn finish(self) {
        if self.failures.is_empty() {
            eprintln!("\ncanary 全绿：{} 项", self.checked);
            return;
        }
        panic!(
            "canary 报告（{} 项里 {} 项失败）\n{}",
            self.checked,
            self.failures.len(),
            self.failures.join("\n")
        );
    }
}

fn stage_of(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::Parse => "解析（**X 可能改版了**）",
        ErrorCode::Unauthorized => "凭据（cookie 过期/被登出）",
        ErrorCode::RateLimited => "限流（等冷却，别重试）",
        ErrorCode::Transport => "网络 / 代理",
        ErrorCode::Upstream => "上游 HTTP 状态",
        ErrorCode::NotFound => "上游说没有这条数据",
        ErrorCode::Cancelled => "被取消",
        _ => "请求构造 / 参数",
    }
}

fn preview(text: &str) -> String {
    text.chars().take(300).collect()
}

/// 失败时把原始响应存进 fixtures（`docs/04` §6 的固定动作）。
async fn save_field_changed(client: &FetchClient, endpoint: &str, call: RawCall<'_>) {
    let cancel = CancelToken::new();
    match client.raw(call, &cancel).await {
        Ok(resp) => {
            let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures")
                .join(endpoint);
            let _ = std::fs::create_dir_all(&dir);
            let path = dir.join(format!("field_changed_{}.json", xdate::today_utc()));
            let body = resp.body_text();
            let body_value: serde_json::Value =
                serde_json::from_str(&body).unwrap_or(serde_json::Value::String(body));
            let envelope = serde_json::json!({
                "endpoint": endpoint,
                "scenario": format!("field_changed_{}", xdate::today_utc()),
                "captured_at": xdate::today_utc(),
                "match_hint": serde_json::Value::Null,
                "note": "canary 失败时自动保存的原始响应（**未脱敏**，入库前必须跑 script/redact_fixtures.py）",
                "response": { "status": resp.status, "headers": {}, "body": body_value },
            });
            match std::fs::write(&path, serde_json::to_string_pretty(&envelope).unwrap()) {
                Ok(()) => eprintln!("     已保存原始响应：{}", path.display()),
                Err(e) => eprintln!("     保存原始响应失败：{e}"),
            }
            eprintln!("     原始响应前 300 字符：{}", preview(&resp.body_text()));
        }
        Err(e) => eprintln!("     重新抓取原始响应也失败了：{e}"),
    }
}

/// 把一次调用收敛成"成功摘要 / 失败报告"。
fn check<T>(
    report: &mut Report,
    endpoint: &str,
    result: XResult<T>,
    summary: impl FnOnce(&T) -> String,
    expectation: impl FnOnce(&T) -> Option<String>,
) -> Option<T> {
    match result {
        Ok(value) => {
            if let Some(problem) = expectation(&value) {
                report.fail(endpoint, "字段校验", problem);
                None
            } else {
                report.pass(endpoint, summary(&value));
                Some(value)
            }
        }
        Err(e) => {
            let context = e.to_object().context.unwrap_or_else(|| "<无>".to_string());
            report.fail(
                endpoint,
                stage_of(e.code()),
                format!("code={:?} context={context} message={e}", e.code()),
            );
            None
        }
    }
}

#[tokio::test]
#[ignore = "live canary：需要 XSPIDER_LIVE=1 与真实 cookie，会消耗账号配额"]
async fn canary_all_endpoints() {
    if std::env::var("XSPIDER_LIVE").as_deref() != Ok("1") {
        eprintln!("跳过：需要 XSPIDER_LIVE=1");
        return;
    }
    let Some(creds) = credentials_from_env() else {
        eprintln!("跳过：需要 XSPIDER_COOKIE");
        return;
    };

    let target =
        std::env::var("XSPIDER_CANARY_SCREEN_NAME").unwrap_or_else(|_| "tesla".to_string());
    let stack = HttpStack::live(proxy_from_env()).expect("构造 HTTP 栈失败");
    stack.set_credentials(Some(creds));
    let client = FetchClient::new(stack);
    let cancel = CancelToken::new();
    let mut report = Report::new();

    // 1) 用户（同时给后面几个端点提供 user_id）
    let user = check(
        &mut report,
        "fetch.get_user",
        client.get_user(&target, &cancel).await,
        |u| {
            format!(
                "id={} screen_name={} media_count={:?} register_time={:?}",
                u.id, u.screen_name, u.media_count, u.register_time
            )
        },
        |u| {
            let mut missing = Vec::new();
            if u.id.is_empty() {
                missing.push("id");
            }
            if u.screen_name.is_empty() {
                missing.push("screen_name");
            }
            if u.name.is_empty() {
                missing.push("name");
            }
            if !u.avatar.starts_with("https://") {
                missing.push("avatar");
            }
            (!missing.is_empty()).then(|| format!("关键字段缺失或形态不对：{missing:?}"))
        },
    );
    let Some(user) = user else {
        // 用户都取不到，后面的端点没有 user_id 可用——给一份完整报告后退出
        report.finish();
        return;
    };
    let user_id = user.id.clone();

    // 2) 媒体时间线（单页）
    let medias = check(
        &mut report,
        "fetch.user_medias",
        client.user_medias(&user_id, None, None, &cancel).await,
        |page| {
            format!(
                "{} 条，cursor={}，end={}",
                page.items.len(),
                if page.cursor.is_some() { "有" } else { "无" },
                page.end
            )
        },
        |page| {
            (page.items.is_empty())
                .then(|| "首页 0 条（该账号没有媒体？还是解析退化了？）".to_string())
        },
    );
    if let Some(page) = &medias {
        match page.items.first() {
            None => {}
            Some(post) if post.medias.is_empty() => report.fail(
                "fetch.user_medias",
                "字段校验",
                format!("第 1 条（id={}）没有 medias——媒体时间线里不该出现", post.id),
            ),
            Some(post) if post.author.screen_name.is_empty() => report.fail(
                "fetch.user_medias",
                "字段校验",
                format!("第 1 条（id={}）没有作者", post.id),
            ),
            Some(_) => {}
        }
    }
    if medias.is_none() {
        save_field_changed(
            &client,
            "user_medias",
            RawCall::UserMedias {
                user_id: &user_id,
                cursor: None,
                count: Some(20),
            },
        )
        .await;
    }

    // 3) 推文时间线（宽松模式：只要能拿到条目就说明解析没坏）
    let tweets = check(
        &mut report,
        "fetch.user_tweets",
        client
            .user_tweets(&user_id, None, None, Some(false), Some(false), &cancel)
            .await,
        |page| format!("{} 条，end={}", page.items.len(), page.end),
        |page| (page.items.is_empty()).then(|| "首页 0 条（宽松模式仍为空，可疑）".to_string()),
    );
    if tweets.is_none() {
        save_field_changed(
            &client,
            "user_tweets",
            RawCall::UserTweets {
                user_id: &user_id,
                cursor: None,
                count: Some(20),
            },
        )
        .await;
    }

    // 4) 推文详情：拿媒体时间线里第一条来查
    match medias
        .as_ref()
        .and_then(|page| page.items.first())
        .map(|p| p.id.clone())
    {
        Some(post_id) => {
            let detail = check(
                &mut report,
                "fetch.tweet_detail",
                client.tweet_detail(&post_id, &cancel).await,
                |detail| {
                    format!(
                        "focal={} 回复 {} 条（孤儿 {} 条），cursor={}",
                        detail.focal.id,
                        detail.replies.len(),
                        detail
                            .replies
                            .iter()
                            .filter(|r| r.is_partial_parent)
                            .count(),
                        if detail.cursor.is_some() {
                            "有"
                        } else {
                            "无"
                        }
                    )
                },
                |detail| {
                    (detail.focal.id != post_id).then(|| {
                        format!(
                            "focal 不是请求的那条：请求 {post_id}，回来 {}",
                            detail.focal.id
                        )
                    })
                },
            );
            if detail.is_none() {
                save_field_changed(
                    &client,
                    "tweet_detail",
                    RawCall::TweetDetail { id: &post_id },
                )
                .await;
            }
        }
        None => report.fail(
            "fetch.tweet_detail",
            "跳过",
            "没有可用的推文 id（上游那一步已经失败）",
        ),
    }

    // 5) 搜索（最近 7 天，只要媒体）
    let today = xdate::today_utc();
    let since = xdate::add_days(&today, -7).unwrap_or_else(|| today.clone());
    let search = check(
        &mut report,
        "fetch.search_timeline",
        client
            .search_timeline(&target, &since, &today, true, None, &cancel)
            .await,
        |page| {
            format!(
                "{} 条（{since} ~ {today}），end={}",
                page.items.len(),
                page.end
            )
        },
        // 搜索本来就可能没结果（这一周该账号没发媒体）——**不算失败**，
        // 但要能看出"确实是空的"而不是"解析坏了"
        |_page| None,
    );
    if search.is_none() {
        save_field_changed(
            &client,
            "search_timeline",
            RawCall::SearchTimeline {
                screen_name: &target,
                since: &since,
                until: &today,
                media_only: true,
                cursor: None,
                count: Some(20),
            },
        )
        .await;
    }

    // 6) 主页时间线（两种模式）
    for mode in ["for_you", "following"] {
        let label = if mode == "for_you" {
            "fetch.home_timeline(for_you)"
        } else {
            "fetch.home_timeline(following)"
        };
        let outcome = check(
            &mut report,
            label,
            client.home_timeline(mode, None, &cancel).await,
            |page| format!("{} 条，end={}", page.items.len(), page.end),
            |page| (page.items.is_empty()).then(|| "首页 0 条，可疑".to_string()),
        );
        if outcome.is_none() {
            save_field_changed(
                &client,
                "home_timeline",
                RawCall::HomeTimeline { mode, cursor: None },
            )
            .await;
        }
    }

    // 7) 关注列表
    let following = check(
        &mut report,
        "fetch.following",
        client.following(&user_id, None, None, &cancel).await,
        |page| format!("{} 人，end={}", page.items.len(), page.end),
        |page| (page.items.is_empty()).then(|| "0 人，可疑".to_string()),
    );
    if following.is_none() {
        save_field_changed(
            &client,
            "following",
            RawCall::Following {
                user_id: &user_id,
                cursor: None,
                count: Some(100),
            },
        )
        .await;
    }

    report.finish();
}

/// 另一条 canary：**不带凭据**时上游必须给出 401/403。
///
/// 这条钉的是"错误分类不能骗人"：如果哪天匿名请求变成 200，
/// 外壳的"要不要提示重新登录"就永远不弹。
#[tokio::test]
#[ignore = "live canary：需要 XSPIDER_LIVE=1 与真实 cookie"]
async fn canary_anonymous_request_is_unauthorized() {
    if std::env::var("XSPIDER_LIVE").as_deref() != Ok("1") {
        eprintln!("跳过：需要 XSPIDER_LIVE=1");
        return;
    }
    let stack = HttpStack::live(proxy_from_env()).expect("构造 HTTP 栈失败");
    stack.set_credentials(Some(
        Credentials::from_cookie("auth_token=invalid; ct0=invalid", None).unwrap(),
    ));
    let client = FetchClient::new(stack);
    let cancel = CancelToken::new();

    let err = client
        .get_user("jack", &cancel)
        .await
        .err()
        .unwrap_or_else(|| panic!("无效凭据竟然成功了——错误分类已失效"));
    assert!(
        matches!(
            err,
            xspider_core::error::XError::Unauthorized { .. }
                | xspider_core::error::XError::Transport { .. }
                | xspider_core::error::XError::Parse { .. }
                | xspider_core::error::XError::Upstream { .. }
        ),
        "无效凭据的返回必须是结构化错误，实际：{err:?}"
    );
    eprintln!("canary（无效凭据）→ {:?}: {err}", err.code());
}
