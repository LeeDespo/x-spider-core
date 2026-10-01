//! 社交与账户：**我是谁**（`auth.whoami`）、**关注态与关注/取关**、**点赞/转推/书签**。
//!
//! 这三件事凑在一个模块里，是因为它们此前是外壳里最后一块"自己发请求"的地方
//! （`NetworkClient` + `XClientTransaction` + `RequestGate` 全靠它们活着）。
//! 搬进来之后，外壳就**完全没有自己的 HTTP 层**了——这是"抽取"这件事的终点形态。
//!
//! # 与参考实现逐字对齐的三处形状（每处都踩过）
//!
//! 1. **突变是 POST + `variables` 在 query 上、不带 body、不带 features**；
//!    搜索那种"POST + JSON body"是另一回事（`docs/02` §A2），混用会 404/400；
//! 2. **关注/取关走 `api.twitter.com` 的 v1.1 form POST**——x.com 域名对该端点 401
//!    （参考实现带着这条实测注释），字段是 `screen_name` + `skip_status=true`；
//! 3. **账户信息靠抓首页 HTML 正则**：`screen_name` / `profile_image_url_https` / `rest_id`。
//!    页面里**没有** `screen_name` 就是 cookie 失效——那是 `unauthorized`，不是 parse 错误
//!    （`docs/02` §A5 那类"看状态码 + 结构"的分类纪律）。

use std::sync::Mutex;

use serde_json::{json, Value};

use xspider_core::cancel::CancelToken;
use xspider_core::error::{XError, XResult};
use xspider_core::ratelimit::RequestClass;
use xspider_core::stack::API_HOST;

use crate::endpoints as ep;
use crate::user::FetchClient;

/// 逻辑端点名（错误里用它，**不用 URL 路径**）。
pub const ENDPOINT_ACCOUNT: &str = "account_info";
pub const ENDPOINT_MUTATE: &str = "mutate";
pub const ENDPOINT_IS_FOLLOWING: &str = "is_following";

/// 当前登录的账号。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Account {
    pub screen_name: String,
    pub avatar: String,
    /// 数字 id；首面 HTML 里偶尔没有，缺失就是 `null`（外壳可以再用 `fetch.get_user` 补）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// 可以施加在推文上的动作。**字符串即契约取值**（`fetch.mutate` 的 `action`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TweetAction {
    Favorite,
    Unfavorite,
    Retweet,
    Unretweet,
    Bookmark,
    Unbookmark,
}

impl TweetAction {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "favorite" => Some(Self::Favorite),
            "unfavorite" => Some(Self::Unfavorite),
            "retweet" => Some(Self::Retweet),
            "unretweet" => Some(Self::Unretweet),
            "bookmark" => Some(Self::Bookmark),
            "unbookmark" => Some(Self::Unbookmark),
            _ => None,
        }
    }

    /// 契约里接受的**全部**取值（错误信息与 schema 都用它，避免两处各写一份）。
    pub const ALL: &'static [&'static str] = &[
        "favorite",
        "unfavorite",
        "retweet",
        "unretweet",
        "bookmark",
        "unbookmark",
        "follow",
        "unfollow",
    ];

    /// 突变用的 `variables`。**逐字来自参考实现**（少一个字段就是 400）。
    fn variables(self, tweet_id: &str) -> Value {
        match self {
            // 这四条一个字段都不能少：`dark_request` 缺失时 X 会拒
            Self::Favorite | Self::Bookmark => json!({ "tweet_id": tweet_id }),
            Self::Unfavorite | Self::Retweet => {
                json!({ "tweet_id": tweet_id, "dark_request": false })
            }
            // 撤销转推的字段名**不一样**：是 `source_tweet_id`
            Self::Unretweet => json!({ "source_tweet_id": tweet_id, "dark_request": false }),
            Self::Unbookmark => json!({ "tweet_id": tweet_id }),
        }
    }

    fn endpoint(self) -> ep::GraphQlEndpoint {
        match self {
            Self::Favorite => ep::FAVORITE_TWEET,
            Self::Unfavorite => ep::UNFAVORITE_TWEET,
            Self::Retweet => ep::CREATE_RETWEET,
            Self::Unretweet => ep::DELETE_RETWEET,
            Self::Bookmark => ep::CREATE_BOOKMARK,
            Self::Unbookmark => ep::DELETE_BOOKMARK,
        }
    }
}

impl FetchClient {
    /// 我是谁：抓一次 x.com 首页，从页面里取 `screen_name` / 头像 / `rest_id`。
    ///
    /// **页面里没有 `screen_name` = 凭据失效**，报 `unauthorized`（外壳据此提示重新登录）；
    /// 不是 parse 错误——X 用"空数据"表达这类状态是常态（`docs/02` §A5）。
    pub async fn whoami(&self, cancel: &CancelToken) -> XResult<Account> {
        let html = self
            .stack()
            // 首页是**公开页面**：会 307 到登录页，所以要跟随重定向；带凭据才拿得到账号信息
            .fetch_text_with(API_HOST, true, RequestClass::Cdn, cancel)
            .await
            .map_err(|e| e.with_endpoint(ENDPOINT_ACCOUNT))?;

        let screen_name = extract(&html, r#""screen_name":"(.*?)""#).ok_or_else(|| {
            XError::unauthorized("首页里没有 screen_name：cookie 可能已失效")
                .with_endpoint(ENDPOINT_ACCOUNT)
        })?;
        let avatar = extract(&html, r#""profile_image_url_https":"(.*?)""#).unwrap_or_default();
        let id = extract(&html, r#""rest_id":"(\d+)""#);
        Ok(Account {
            screen_name,
            avatar,
            id,
        })
    }

    /// 我有没有关注这个用户。
    ///
    /// `friendships/show` 需要**两边的 screen_name**（v1.1 的形状），所以这里要
    /// 先知道"我是谁"。为了不让每次查询都多花一个请求，结果缓存在客户端里，
    /// 由 `auth.set_cookie` 清掉（换账号之后"我是谁"就变了）。
    pub async fn is_following(&self, screen_name: &str, cancel: &CancelToken) -> XResult<bool> {
        let me = self.current_screen_name(cancel).await?;
        let body = self
            .stack()
            .rest_v1_get(
                ENDPOINT_IS_FOLLOWING,
                ep::FRIENDSHIPS_SHOW,
                &[
                    ("source_screen_name".to_string(), me),
                    ("target_screen_name".to_string(), screen_name.to_string()),
                ],
                cancel,
            )
            .await?;
        // `relationship.source.following` = 我有没有关注对方。结构对不上就报 parse
        // （那是"X 改版了"的信号，不能默默返回 false——那会让界面显示错误的关注态）。
        body.pointer("/relationship/source/following")
            .and_then(Value::as_bool)
            .ok_or_else(|| {
                XError::parse(
                    "is_following.relationship.source.following",
                    "friendships/show 的响应里没有 relationship.source.following；X 可能改版了",
                )
                .with_endpoint(ENDPOINT_IS_FOLLOWING)
            })
    }

    /// 点赞 / 取消 / 转推 / 撤销 / 书签 / 移除书签 / 关注 / 取关。
    ///
    /// **这是写操作**：动的是用户的真实账号。所以：
    /// - 参数校验放在最前面（缺 `tweet_id` 直接 `invalid_request`，不发请求）；
    /// - 任何失败都**原样上报**（不吞、不改写成"成功"）——外壳要能据此告诉用户"没生效"。
    pub async fn mutate(
        &self,
        action: &str,
        tweet_id: Option<&str>,
        screen_name: Option<&str>,
        cancel: &CancelToken,
    ) -> XResult<()> {
        match action {
            "follow" | "unfollow" => {
                let screen_name = screen_name.ok_or_else(|| {
                    XError::invalid_request("action=follow/unfollow 需要 screen_name")
                        .with_endpoint(ENDPOINT_MUTATE)
                })?;
                let path = if action == "follow" {
                    ep::FRIENDSHIPS_CREATE
                } else {
                    ep::FRIENDSHIPS_DESTROY
                };
                let body = self
                    .stack()
                    .rest_v1_form_post(
                        ENDPOINT_MUTATE,
                        path,
                        &[
                            ("screen_name".to_string(), screen_name.to_string()),
                            ("skip_status".to_string(), "true".to_string()),
                        ],
                        cancel,
                    )
                    .await?;
                ensure_mutation_succeeded(ENDPOINT_MUTATE, &body)
            }
            other => {
                let action = TweetAction::parse(other).ok_or_else(|| {
                    XError::invalid_request(format!(
                        "未知 action：{other}（可用：{}）",
                        TweetAction::ALL.join(" / ")
                    ))
                    .with_endpoint(ENDPOINT_MUTATE)
                })?;
                let tweet_id = tweet_id.ok_or_else(|| {
                    XError::invalid_request(format!("action={other} 需要 tweet_id"))
                        .with_endpoint(ENDPOINT_MUTATE)
                })?;
                let endpoint = action.endpoint();
                let body = self
                    .stack()
                    .graphql_mutate(
                        ENDPOINT_MUTATE,
                        &endpoint.path(),
                        &action.variables(tweet_id),
                        cancel,
                    )
                    .await?;
                ensure_mutation_succeeded(ENDPOINT_MUTATE, &body)
            }
        }
    }

    /// "我是谁"的缓存（只在进程内、由换 cookie 清掉）。
    async fn current_screen_name(&self, cancel: &CancelToken) -> XResult<String> {
        if let Some(cached) = self
            .account_cache()
            .lock()
            .expect("account cache poisoned")
            .clone()
        {
            return Ok(cached);
        }
        let account = self.whoami(cancel).await?;
        *self.account_cache().lock().expect("account cache poisoned") =
            Some(account.screen_name.clone());
        Ok(account.screen_name)
    }

    /// 换 cookie 时必须调用：否则会拿旧账号的身份去查关注态。
    pub fn invalidate_account_cache(&self) {
        *self.account_cache().lock().expect("account cache poisoned") = None;
    }
}

/// **写操作的成败要看返回体，不能只看 HTTP 状态码。**
///
/// X 的 GraphQL 突变失败时返回的是 **HTTP 200 + `{"errors":[…]}`**：
/// 只看状态码会把"没做成"报成成功，于是在界面上留下一个"已点赞"的假象
/// （而服务端什么都没发生）。v1.1 REST 同理。
///
/// 判定只用**结构化字段**：`errors[].code`（数字，X 的公开错误码）与 `data` 的存在性。
/// 数字码之外的失败归到 `upstream`，并把码带在消息里做诊断——**不匹配文案**。
fn ensure_mutation_succeeded(endpoint: &'static str, body: &Value) -> XResult<()> {
    if let Some(errors) = body.get("errors").and_then(Value::as_array) {
        if let Some(first) = errors.first() {
            let code = first.get("code").and_then(Value::as_i64);
            // 实测遇到过的三类：
            // - 144（GraphQL：没有这条推文）、34（v1.1：页面不存在）→ not_found；
            // - 32/89/99/215（认证类）、以及 **141**（"User is suspended, deactivated or
            //   offboarded"——账号被限制写操作）→ unauthorized。
            //   141 归到这里，是因为**外壳该做的事与登录失效完全一样**：提示换账号 / 重新登录，
            //   而不是当成一个让人去查文档的 upstream。
            //   实测证据：一个 0 推文、0 关注的新账号，读全部正常（whoami / 时间线 / 详情），
            //   只有写操作稳定返回 141——所以那不是请求构造错了，是账号本身不能写。
            let message = first
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("写操作被拒")
                .to_string();
            let error = match code {
                Some(144) | Some(34) => XError::not_found(message),
                Some(32) | Some(89) | Some(99) | Some(215) | Some(141) => {
                    XError::unauthorized(message)
                }
                _ => XError::upstream(
                    200,
                    match code {
                        Some(code) => format!("{message}（上游错误码 {code}）"),
                        None => message,
                    },
                ),
            };
            return Err(error.with_endpoint(endpoint));
        }
    }
    // 既没有 errors 也没有 data：形状不对，那是"X 改版了"的信号（parse）
    if body.get("data").is_none() {
        return Err(XError::parse(
            "mutate.data",
            "写操作的响应既没有 data 也没有 errors；X 可能改版了",
        )
        .with_endpoint(endpoint));
    }
    Ok(())
}

fn extract(html: &str, pattern: &str) -> Option<String> {
    static CACHE: Mutex<Vec<(String, regex::Regex)>> = Mutex::new(Vec::new());
    let regex = {
        let mut cache = CACHE.lock().expect("regex cache poisoned");
        if let Some((_, re)) = cache.iter().find(|(p, _)| p == pattern) {
            re.clone()
        } else {
            let re = regex::Regex::new(pattern).ok()?;
            cache.push((pattern.to_string(), re.clone()));
            re
        }
    };
    regex
        .captures(html)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tweet_actions_parse_and_expose_every_contract_value() {
        for raw in TweetAction::ALL {
            if let Some(action) = TweetAction::parse(raw) {
                assert!(!action.variables("1").is_null());
            } else {
                assert!(matches!(*raw, "follow" | "unfollow"), "{raw} 没被解析");
            }
        }
        assert!(TweetAction::parse("like").is_none(), "不接受自造的名字");
    }

    /// 每条突变的 `variables` 形状逐字来自参考实现，**少一个字段就是 400**。
    #[test]
    fn mutation_variables_match_the_reference() {
        assert_eq!(
            TweetAction::Favorite.variables("42"),
            json!({ "tweet_id": "42" })
        );
        assert_eq!(
            TweetAction::Bookmark.variables("42"),
            json!({ "tweet_id": "42" })
        );
        assert_eq!(
            TweetAction::Unfavorite.variables("42"),
            json!({ "tweet_id": "42", "dark_request": false })
        );
        assert_eq!(
            TweetAction::Retweet.variables("42"),
            json!({ "tweet_id": "42", "dark_request": false })
        );
        // 撤销转推的键名与其它都不一样：source_tweet_id
        assert_eq!(
            TweetAction::Unretweet.variables("42"),
            json!({ "source_tweet_id": "42", "dark_request": false })
        );
        assert_eq!(
            TweetAction::Unbookmark.variables("42"),
            json!({ "tweet_id": "42" })
        );
    }

    #[test]
    fn extract_pulls_the_three_fields_from_a_realistic_page() {
        let html = r#"<script>{"user":{"rest_id":"50909294","legacy":{"screen_name":"demo_user"}},
            "profile_image_url_https":"https://pbs.twimg.com/a_normal.jpg"}</script>"#;
        assert_eq!(
            extract(html, r#""screen_name":"(.*?)""#).as_deref(),
            Some("demo_user")
        );
        assert_eq!(
            extract(html, r#""rest_id":"(\d+)""#).as_deref(),
            Some("50909294")
        );
        assert!(extract(r#"<html>登录页</html>"#, r#""screen_name":"(.*?)""#).is_none());
    }
}

/// 写操作的成败判定：**HTTP 200 + errors[] 也是失败**。
/// 这条是被真实使用抓出来的：不检查返回体时，界面上会出现"已点赞"的假象。
#[test]
fn mutation_success_is_decided_by_the_body_not_the_status_code() {
    // 成功：data 在、errors 不在
    assert!(ensure_mutation_succeeded(
        ENDPOINT_MUTATE,
        &json!({ "data": { "favorite_tweet": "Done" } })
    )
    .is_ok());
    // v1.1 的成功形状（data 是用户对象）
    assert!(ensure_mutation_succeeded(
        ENDPOINT_MUTATE,
        &json!({ "data": { "screen_name": "demo_user" } })
    )
    .is_ok());

    // 失败：144 = 没有这条推文 → not_found
    let err = ensure_mutation_succeeded(
        ENDPOINT_MUTATE,
        &json!({ "errors": [{ "code": 144, "message": "No status found with that ID." }] }),
    )
    .unwrap_err();
    assert_eq!(err.code(), xspider_core::error::ErrorCode::NotFound);

    // 失败：认证类 → unauthorized（外壳据此提示重新登录）
    let err = ensure_mutation_succeeded(
        ENDPOINT_MUTATE,
        &json!({ "errors": [{ "code": 32, "message": "Could not authenticate you." }] }),
    )
    .unwrap_err();
    assert_eq!(err.code(), xspider_core::error::ErrorCode::Unauthorized);

    // 141 = "User is suspended, deactivated or offboarded" → 也归 unauthorized。
    // 这条是**实测出来的**：一个 0 推文、0 关注的新账号，读全部正常，
    // 只有写操作稳定返回 141。外壳对这种账号能做的事与登录失效一样（换账号/重新登录），
    // 所以不该让它落进 upstream 变成一个"看起来像服务端出问题"的错误码。
    let err = ensure_mutation_succeeded(
        ENDPOINT_MUTATE,
        &json!({ "errors": [{ "code": 141,
            "message": "Authorization: User (uid: 1) is suspended, deactivated or offboarded" }] }),
    )
    .unwrap_err();
    assert_eq!(err.code(), xspider_core::error::ErrorCode::Unauthorized);

    // 未知码：归 upstream，但**把数字码带出来**（不猜语义、也不丢诊断信息）
    let err = ensure_mutation_succeeded(
        ENDPOINT_MUTATE,
        &json!({ "errors": [{ "code": 261, "message": "boom" }] }),
    )
    .unwrap_err();
    assert_eq!(err.code(), xspider_core::error::ErrorCode::Upstream);
    assert!(err.to_string().contains("261"), "{err}");

    // 形状完全不对 → parse（"X 可能改版了"）
    let err =
        ensure_mutation_succeeded(ENDPOINT_MUTATE, &json!({ "unexpected": true })).unwrap_err();
    assert_eq!(err.code(), xspider_core::error::ErrorCode::Parse);
}
