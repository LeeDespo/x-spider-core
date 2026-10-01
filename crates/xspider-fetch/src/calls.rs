//! **请求构造的唯一实现点**。
//!
//! 三处需要发出同样的请求：端点方法、fixture 录制、canary 存证。
//! 如果各写一遍，录下来的东西就**不代表生产行为**——fixture 会变成"我以为的请求"
//! 而不是"真实发出的请求"（`docs/04-TESTING-AND-FIXTURES.md` §2.1 正是要避免这个）。
//! 所以这里把「一次调用」抽象成 [`RawCall`] → [`PreparedCall`]，三处共用。
//!
//! # 两条纪律写在这里，因为这里只有一个实现点
//!
//! 1. **`cursor` 缺省时整个键省略，不是传 `null`**（`docs/02` §A1：传 null 会让每一页
//!    都请求第一页，表现为无限加载 / 重复抓同一页）；
//! 2. **`variables` 的键序与上游一致**（配合 `serde_json` 的 `preserve_order`），
//!    `cursor` 排在最后——这样录下来的请求与上游逐字可比。

use serde_json::Value;

use xspider_core::error::{XError, XResult};
use xspider_core::transport::HttpMethod;

use crate::endpoints::{
    self, GraphQlEndpoint, FOLLOWING, SEARCH_TIMELINE, USER_BY_SCREEN_NAME, USER_MEDIA, USER_TWEETS,
};
use crate::search_query_id::SearchQueryIdProvider;

/// 上游的默认页大小。
pub(crate) const DEFAULT_COUNT: u64 = 20;
/// `following` 的默认页大小（上游用 100，不是 20）。
pub(crate) const DEFAULT_FOLLOWING_COUNT: u64 = 100;

/// 逻辑端点名。错误与日志里用它，**不用 URL 路径**。
pub const ENDPOINT_USER_MEDIAS: &str = "user_medias";
pub const ENDPOINT_USER_TWEETS: &str = "user_tweets";
pub const ENDPOINT_TWEET_DETAIL: &str = "tweet_detail";
pub const ENDPOINT_SEARCH_TIMELINE: &str = "search_timeline";
pub const ENDPOINT_HOME_TIMELINE: &str = "home_timeline";
pub const ENDPOINT_FOLLOWING: &str = "following";

/// 一次调用的参数。与契约的 `params` 一一对应。
#[derive(Debug, Clone)]
pub enum RawCall<'a> {
    UserByScreenName {
        screen_name: &'a str,
    },
    UserMedias {
        user_id: &'a str,
        cursor: Option<&'a str>,
        count: Option<u64>,
    },
    UserTweets {
        user_id: &'a str,
        cursor: Option<&'a str>,
        count: Option<u64>,
    },
    TweetDetail {
        id: &'a str,
    },
    SearchTimeline {
        screen_name: &'a str,
        since: &'a str,
        until: &'a str,
        media_only: bool,
        cursor: Option<&'a str>,
        count: Option<u64>,
    },
    HomeTimeline {
        mode: &'a str,
        cursor: Option<&'a str>,
    },
    Following {
        user_id: &'a str,
        cursor: Option<&'a str>,
        count: Option<u64>,
    },
}

impl RawCall<'_> {
    /// 逻辑端点名（错误上下文用）。
    pub fn endpoint(&self) -> &'static str {
        match self {
            RawCall::UserByScreenName { .. } => crate::user::ENDPOINT,
            RawCall::UserMedias { .. } => ENDPOINT_USER_MEDIAS,
            RawCall::UserTweets { .. } => ENDPOINT_USER_TWEETS,
            RawCall::TweetDetail { .. } => ENDPOINT_TWEET_DETAIL,
            RawCall::SearchTimeline { .. } => ENDPOINT_SEARCH_TIMELINE,
            RawCall::HomeTimeline { .. } => ENDPOINT_HOME_TIMELINE,
            RawCall::Following { .. } => ENDPOINT_FOLLOWING,
        }
    }

    /// 组装成可以发出去的请求。
    ///
    /// `search_query_ids` 只在搜索端点用得上（queryId 会失效，需要自愈）。
    pub fn prepare(&self, search_query_ids: &SearchQueryIdProvider) -> XResult<PreparedCall> {
        match self {
            RawCall::UserByScreenName { screen_name } => {
                // 用 normalize 而不是 require_non_empty：外壳可能把 "@jack" 直接传进来，
                // 而 X 的 variables 里不能带 @
                let screen_name = crate::user::normalize_screen_name(screen_name)?;
                let variables = serde_json::json!({
                    "screen_name": screen_name,
                    "withSafetyModeUserFields": true,
                });
                Ok(PreparedCall {
                    endpoint: crate::user::ENDPOINT,
                    method: HttpMethod::Get,
                    path: USER_BY_SCREEN_NAME.path(),
                    query: vec![
                        (
                            "features".to_string(),
                            USER_BY_SCREEN_NAME.features.to_string(),
                        ),
                        (
                            "fieldToggles".to_string(),
                            endpoints::USER_BY_SCREEN_NAME_FIELD_TOGGLES.to_string(),
                        ),
                        ("variables".to_string(), variables.to_string()),
                    ],
                    body: None,
                })
            }
            RawCall::UserMedias {
                user_id,
                cursor,
                count,
            } => {
                let variables = with_cursor(
                    serde_json::json!({
                        "userId": require_non_empty(user_id, "user_id")?,
                        "count": count.unwrap_or(DEFAULT_COUNT),
                        "includePromotedContent": false,
                        "withClientEventToken": false,
                        "withBirdwatchNotes": false,
                        "withVoice": true,
                        "withV2Timeline": true,
                    }),
                    *cursor,
                );
                Ok(PreparedCall::get(
                    ENDPOINT_USER_MEDIAS,
                    USER_MEDIA,
                    &variables,
                ))
            }
            RawCall::UserTweets {
                user_id,
                cursor,
                count,
            } => {
                let variables = with_cursor(
                    serde_json::json!({
                        "userId": require_non_empty(user_id, "user_id")?,
                        "count": count.unwrap_or(DEFAULT_COUNT),
                        "includePromotedContent": true,
                        "withQuickPromoteEligibilityTweetFields": true,
                        "withVoice": true,
                        "withV2Timeline": true,
                    }),
                    *cursor,
                );
                Ok(PreparedCall::get(
                    ENDPOINT_USER_TWEETS,
                    USER_TWEETS,
                    &variables,
                ))
            }
            RawCall::TweetDetail { id } => {
                // TweetDetail **没有 cursor**（不分页，docs/02 §C4）
                let variables = serde_json::json!({
                    "focalTweetId": require_non_empty(id, "id")?,
                    "with_rux_injections": true,
                    "includePromotedContent": true,
                    "withCommunity": true,
                    "withQuickPromoteEligibilityTweetFields": true,
                    "withBirdwatchNotes": true,
                    "withVoice": true,
                    "withV2Timeline": true,
                });
                Ok(PreparedCall::get(
                    ENDPOINT_TWEET_DETAIL,
                    endpoints::TWEET_DETAIL,
                    &variables,
                ))
            }
            RawCall::SearchTimeline {
                screen_name,
                since,
                until,
                media_only,
                cursor,
                count,
            } => {
                let raw_query = search_raw_query(screen_name, since, until, *media_only)?;
                let variables = with_cursor(
                    serde_json::json!({
                        "rawQuery": raw_query,
                        "count": count.unwrap_or(DEFAULT_COUNT),
                        "querySource": "typed_query",
                        "product": if *media_only { "Media" } else { "Latest" },
                    }),
                    *cursor,
                );
                let endpoint = SEARCH_TIMELINE;
                let query_id = search_query_ids.current();
                Ok(PreparedCall {
                    endpoint: ENDPOINT_SEARCH_TIMELINE,
                    method: HttpMethod::Post,
                    path: endpoint.path_with(&query_id),
                    query: Vec::new(),
                    // 注意：`features` 在这里是**JSON 字符串**而不是嵌套对象。
                    // 这是上游实测能通的形态（Swift 版把 features 当 String 传），
                    // 属于"照抄能通的实现"，不要"顺手"改成嵌套对象
                    // （`docs/02` §0 移植纪律）。
                    body: Some(serde_json::json!({
                        "variables": variables,
                        "features": endpoint.features,
                    })),
                })
            }
            RawCall::HomeTimeline { mode, cursor } => {
                let endpoint = endpoints::match_home_mode(mode)?;
                let variables = with_cursor(
                    serde_json::json!({
                        "count": DEFAULT_COUNT,
                        "includePromotedContent": true,
                        "latestControlAvailable": true,
                        "requestContext": "launch",
                    }),
                    *cursor,
                );
                Ok(PreparedCall::get(
                    ENDPOINT_HOME_TIMELINE,
                    endpoint,
                    &variables,
                ))
            }
            RawCall::Following {
                user_id,
                cursor,
                count,
            } => {
                let variables = with_cursor(
                    serde_json::json!({
                        "userId": require_non_empty(user_id, "user_id")?,
                        "count": count.unwrap_or(DEFAULT_FOLLOWING_COUNT),
                        "includePromotedContent": false,
                    }),
                    *cursor,
                );
                Ok(PreparedCall::get(ENDPOINT_FOLLOWING, FOLLOWING, &variables))
            }
        }
    }
}

/// 组装好的请求。`path` 里含 queryId——**它是组件实现，绝不可进契约**
/// （公开出来只是为了让录制与排障能看到"真实发了什么"）。
#[derive(Debug, Clone)]
pub struct PreparedCall {
    pub endpoint: &'static str,
    pub method: HttpMethod,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub body: Option<Value>,
}

impl PreparedCall {
    fn get(endpoint: &'static str, spec: GraphQlEndpoint, variables: &Value) -> Self {
        debug_assert_eq!(spec.method, HttpMethod::Get, "只有搜索是 POST");
        Self {
            endpoint,
            method: HttpMethod::Get,
            path: spec.path(),
            query: graphql_query(spec.features, variables),
            body: None,
        }
    }
}

/// 组装 GET 端点的查询参数：`features` + `variables`（顺序与上游一致）。
pub(crate) fn graphql_query(features: &str, variables: &Value) -> Vec<(String, String)> {
    vec![
        ("features".to_string(), features.to_string()),
        ("variables".to_string(), variables.to_string()),
    ]
}

/// **`cursor` 缺省时整个键不出现**（`docs/02` §A1）。这是那条纪律的唯一实现点。
pub(crate) fn with_cursor(mut variables: Value, cursor: Option<&str>) -> Value {
    if let (Some(cursor), Some(map)) = (cursor, variables.as_object_mut()) {
        if !cursor.is_empty() {
            map.insert("cursor".to_string(), Value::String(cursor.to_string()));
        }
    }
    variables
}

fn require_non_empty(value: &str, field: &str) -> XResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(XError::invalid_request(format!("{field} 不能为空")));
    }
    Ok(trimmed.to_string())
}

/// 组装搜索的 `rawQuery`。
///
/// 形如 `from:<用户名> since:<起始日> until:<结束日 + 1 天>`，
/// `media_only` 时追加 ` filter:media`（服务端筛选，比客户端筛省请求）。
///
/// 日期语义见 `xspider_core::xdate::search_date_range`：
/// 契约给的是**本地日历日期**，`until` 排他所以 +1 天，**只加一次**。
pub(crate) fn search_raw_query(
    screen_name: &str,
    since: &str,
    until: &str,
    media_only: bool,
) -> XResult<String> {
    // `from:@user` 是错的；同样要归一化掉 @
    let screen_name = crate::user::normalize_screen_name(screen_name)?;
    let (since, until) = xspider_core::xdate::search_date_range(since, until).ok_or_else(|| {
        XError::invalid_request(format!(
            "since/until 必须是 YYYY-MM-DD 形式的真实日期（收到 since={since:?} until={until:?}）"
        ))
    })?;
    let mut query = format!("from:{screen_name} since:{since} until:{until}");
    if media_only {
        query.push_str(" filter:media");
    }
    Ok(query)
}

// ---------------------------------------------------------------------------
// 出网：唯一入口
// ---------------------------------------------------------------------------

impl crate::FetchClient {
    /// 发出一次调用并返回解析后的 JSON。**这是唯一的出网入口**
    /// （端点方法、录制、canary 都走它），所以限流、签名、自愈都不会被绕过。
    ///
    /// 搜索端点的 404 会触发一次自愈重试：只有它需要（`docs/02` §A2/A3）。
    pub(crate) async fn call_json(
        &self,
        call: RawCall<'_>,
        cancel: &xspider_core::cancel::CancelToken,
    ) -> XResult<Value> {
        let endpoint = call.endpoint();
        let is_search = matches!(call, RawCall::SearchTimeline { .. });

        match self.send_prepared(&call, cancel).await {
            Ok(value) => Ok(value),
            // 搜索 404 → 自愈一次。别的端点 404 不是 queryId 问题（docs/02 §A2），
            // 所以只在搜索上做这件事，避免把"用户不存在"之类误诊成改版。
            Err(err) if is_search && matches!(err, XError::Upstream { status: 404, .. }) => {
                tracing::warn!("{endpoint} 返回 404，尝试自愈 queryId");
                let fetch = self.page_fetcher(cancel);
                match self.search_query_ids().refresh(fetch).await {
                    Ok(Some(_)) => self.send_prepared(&call, cancel).await,
                    Ok(None) => Err(err),
                    Err(heal_err) => Err(XError::Parse {
                        context: format!("{endpoint}.query_id"),
                        message: format!("搜索 404 且自愈失败：{heal_err}"),
                        endpoint: Some(endpoint.to_string()),
                    }),
                }
            }
            Err(err) => Err(err),
        }
    }

    async fn send_prepared(
        &self,
        call: &RawCall<'_>,
        cancel: &xspider_core::cancel::CancelToken,
    ) -> XResult<Value> {
        let prepared = call.prepare(self.search_query_ids())?;
        match prepared.method {
            HttpMethod::Get => {
                self.stack()
                    .graphql_get(prepared.endpoint, &prepared.path, &prepared.query, cancel)
                    .await
            }
            HttpMethod::Post => {
                let body = prepared.body.as_ref().ok_or_else(|| {
                    XError::internal(format!("{} 是 POST 但没有 body", prepared.endpoint))
                })?;
                self.stack()
                    .graphql_post(prepared.endpoint, &prepared.path, body, cancel)
                    .await
            }
            // X 的 GraphQL 端点只有 GET 与 POST；HEAD 是给"问媒体多大"用的
            // （`HttpStack::probe_size`），不经过取数组件。
            HttpMethod::Head => Err(XError::internal(format!(
                "{} 是 GraphQL 端点，不该发 HEAD",
                prepared.endpoint
            ))),
        }
    }

    /// 供自愈用：抓搜索页与 bundle。
    ///
    /// 两类请求都按 **CDN 配额**过闸门（大头是 `abs.twimg.com` 的资源），
    /// 且页面那一条会带上凭据——原因见 `crate::search_query_id` 的模块头注释第 3 条。
    fn page_fetcher(
        &self,
        cancel: &xspider_core::cancel::CancelToken,
    ) -> crate::search_query_id::PageFetchFn {
        let stack = self.stack().clone();
        let cancel = cancel.clone();
        std::sync::Arc::new(move |url: String, with_credentials: bool| {
            let stack = stack.clone();
            let cancel = cancel.clone();
            Box::pin(async move {
                stack
                    .fetch_text_with(
                        &url,
                        with_credentials,
                        xspider_core::RequestClass::Cdn,
                        &cancel,
                    )
                    .await
            })
        })
    }

    /// **录制 / 排障专用**：发出与端点方法**完全相同**的请求，但返回原始响应不求解析。
    ///
    /// - 录制 fixture（`docs/04` §2.1 要求 fixture 必须是真实抓到的响应）；
    /// - canary 红了之后把原始响应存成 `field_changed_<日期>.json` 再修解析。
    ///
    /// 走同一条出网路径，所以录下来的东西**代表生产行为**，不是"我以为的请求"。
    pub async fn raw(
        &self,
        call: RawCall<'_>,
        cancel: &xspider_core::cancel::CancelToken,
    ) -> XResult<xspider_core::transport::HttpResponse> {
        let prepared = call.prepare(self.search_query_ids())?;
        let body = prepared.body.clone();
        self.stack()
            .send(
                xspider_core::stack::ApiSpec {
                    endpoint: prepared.endpoint,
                    method: prepared.method,
                    path: &prepared.path,
                    query: &prepared.query,
                    body,
                    form: None,
                    with_credentials: true,
                    // 取数端点全部在 x.com（v1.1 REST 只在社交那几条用 api.twitter.com）
                    host: xspider_core::stack::API_HOST,
                },
                cancel,
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> SearchQueryIdProvider {
        SearchQueryIdProvider::new()
    }

    #[test]
    fn cursor_is_omitted_when_absent_and_last_when_present() {
        // docs/02 §A1：缺省时整个键不出现，不是 null
        let without = with_cursor(serde_json::json!({ "userId": "1" }), None);
        assert!(without.get("cursor").is_none());
        assert!(with_cursor(serde_json::json!({ "userId": "1" }), Some(""))
            .get("cursor")
            .is_none());

        let with = with_cursor(serde_json::json!({ "userId": "1" }), Some("CUR"));
        assert_eq!(with["cursor"], "CUR");
        let keys: Vec<&String> = with.as_object().unwrap().keys().collect();
        assert_eq!(
            keys.last().map(|s| s.as_str()),
            Some("cursor"),
            "cursor 必须最后"
        );
    }

    #[test]
    fn user_medias_variables_match_upstream() {
        let call = RawCall::UserMedias {
            user_id: "42",
            cursor: None,
            count: None,
        }
        .prepare(&provider())
        .unwrap();
        assert_eq!(call.method, HttpMethod::Get);
        assert_eq!(call.endpoint, "user_medias");
        let keys: Vec<&str> = call.query.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["features", "variables"]);
        let variables: Value = serde_json::from_str(&call.query[1].1).unwrap();
        assert_eq!(variables["userId"], "42");
        assert_eq!(variables["count"], 20);
        assert_eq!(variables["includePromotedContent"], false);
        assert_eq!(variables["withVoice"], true);
        assert_eq!(variables["withV2Timeline"], true);
        assert!(variables.get("cursor").is_none(), "首页不能带 cursor");
        // 键序与上游一致
        let vkeys: Vec<&String> = variables.as_object().unwrap().keys().collect();
        assert_eq!(
            vkeys.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            vec![
                "userId",
                "count",
                "includePromotedContent",
                "withClientEventToken",
                "withBirdwatchNotes",
                "withVoice",
                "withV2Timeline"
            ]
        );
    }

    #[test]
    fn user_tweets_variables_match_upstream() {
        let call = RawCall::UserTweets {
            user_id: "42",
            cursor: Some("CUR"),
            count: Some(5),
        }
        .prepare(&provider())
        .unwrap();
        let variables: Value = serde_json::from_str(&call.query[1].1).unwrap();
        assert_eq!(variables["count"], 5);
        assert_eq!(variables["includePromotedContent"], true);
        assert_eq!(variables["withQuickPromoteEligibilityTweetFields"], true);
        assert_eq!(variables["cursor"], "CUR");
    }

    #[test]
    fn tweet_detail_variables_have_no_cursor_and_the_documented_keys() {
        let call = RawCall::TweetDetail { id: "123" }
            .prepare(&provider())
            .unwrap();
        let variables: Value = serde_json::from_str(&call.query[1].1).unwrap();
        assert_eq!(variables["focalTweetId"], "123");
        assert_eq!(variables["with_rux_injections"], true);
        assert_eq!(variables["withCommunity"], true);
        assert_eq!(variables["withBirdwatchNotes"], true);
        assert_eq!(variables.as_object().unwrap().len(), 8);
        assert!(variables.get("cursor").is_none(), "TweetDetail 不分页");
    }

    #[test]
    fn following_uses_a_bigger_default_page() {
        let call = RawCall::Following {
            user_id: "1",
            cursor: None,
            count: None,
        }
        .prepare(&provider())
        .unwrap();
        let variables: Value = serde_json::from_str(&call.query[1].1).unwrap();
        assert_eq!(variables["count"], 100, "following 的默认页是 100，不是 20");
        assert_eq!(variables["includePromotedContent"], false);
    }

    #[test]
    fn home_timeline_modes_pick_different_paths() {
        let for_you = RawCall::HomeTimeline {
            mode: "for_you",
            cursor: None,
        }
        .prepare(&provider())
        .unwrap();
        let following = RawCall::HomeTimeline {
            mode: "following",
            cursor: None,
        }
        .prepare(&provider())
        .unwrap();
        assert!(for_you.path.contains("/HomeTimeline"), "{}", for_you.path);
        assert!(
            following.path.contains("/HomeLatestTimeline"),
            "{}",
            following.path
        );
        assert_ne!(for_you.path, following.path);
        let variables: Value = serde_json::from_str(&for_you.query[1].1).unwrap();
        assert_eq!(variables["requestContext"], "launch");
        assert_eq!(variables["latestControlAvailable"], true);
        assert_eq!(variables["includePromotedContent"], true);
        assert_eq!(variables["count"], 20);
    }

    #[test]
    fn search_is_post_with_features_as_a_json_string() {
        let call = RawCall::SearchTimeline {
            screen_name: "jack",
            since: "2026-01-01",
            until: "2026-01-31",
            media_only: true,
            cursor: None,
            count: None,
        }
        .prepare(&provider())
        .unwrap();
        assert_eq!(call.method, HttpMethod::Post);
        assert!(call.query.is_empty(), "搜索的参数全在 body 里");
        assert!(call.path.contains("/SearchTimeline"), "{}", call.path);

        let body = call.body.unwrap();
        let variables = &body["variables"];
        assert_eq!(
            variables["rawQuery"], "from:jack since:2026-01-01 until:2026-02-01 filter:media",
            "media_only 要交给服务端筛（filter:media），比取回来再筛省请求也省配额"
        );
        assert_eq!(variables["querySource"], "typed_query");
        assert_eq!(variables["product"], "Media");
        assert!(variables.get("cursor").is_none());
        // features 必须是**字符串**（照抄能通的形态，见 docs/02 §0）
        assert!(
            body["features"].is_string(),
            "features 必须以 JSON 字符串形态发出：{}",
            body["features"]
        );
        // 而且那个字符串本身是合法 JSON
        let features: Value = serde_json::from_str(body["features"].as_str().unwrap()).unwrap();
        assert!(features.is_object());
    }

    #[test]
    fn search_product_is_latest_without_media_only() {
        let call = RawCall::SearchTimeline {
            screen_name: "jack",
            since: "2026-01-01",
            until: "2026-01-01",
            media_only: false,
            cursor: None,
            count: None,
        }
        .prepare(&provider())
        .unwrap();
        let body = call.body.unwrap();
        assert_eq!(body["variables"]["product"], "Latest");
        // 不带 filter:media
        assert_eq!(
            body["variables"]["rawQuery"],
            "from:jack since:2026-01-01 until:2026-01-02"
        );
    }

    #[test]
    fn search_raw_query_makes_until_exclusive_exactly_once() {
        assert_eq!(
            search_raw_query("a", "2026-03-01", "2026-03-31", false).unwrap(),
            "from:a since:2026-03-01 until:2026-04-01"
        );
        // 非法日期必须报 invalid_request，而不是默默当成今天
        let err = search_raw_query("a", "2026-02-30", "2026-03-01", false).unwrap_err();
        assert_eq!(err.code(), xspider_core::error::ErrorCode::InvalidRequest);
        assert!(err.to_string().contains("YYYY-MM-DD"), "{err}");
    }

    #[test]
    fn screen_names_are_normalized_at_construction_time() {
        // 外壳把 "@jack" 传进来时，X 的 variables / rawQuery 里都不能出现 @
        let call = RawCall::UserByScreenName {
            screen_name: " @jack ",
        }
        .prepare(&provider())
        .unwrap();
        let variables: Value = serde_json::from_str(&call.query[2].1).unwrap();
        assert_eq!(variables["screen_name"], "jack");

        assert_eq!(
            search_raw_query(" @jack ", "2026-01-01", "2026-01-01", false).unwrap(),
            "from:jack since:2026-01-01 until:2026-01-02"
        );
    }

    #[test]
    fn empty_required_fields_are_rejected_before_any_request() {
        for call in [
            RawCall::UserMedias {
                user_id: "  ",
                cursor: None,
                count: None,
            },
            RawCall::UserTweets {
                user_id: "",
                cursor: None,
                count: None,
            },
            RawCall::TweetDetail { id: "" },
            RawCall::Following {
                user_id: " ",
                cursor: None,
                count: None,
            },
            RawCall::SearchTimeline {
                screen_name: " ",
                since: "2026-01-01",
                until: "2026-01-01",
                media_only: false,
                cursor: None,
                count: None,
            },
        ] {
            let err = call.prepare(&provider()).unwrap_err();
            assert_eq!(
                err.code(),
                xspider_core::error::ErrorCode::InvalidRequest,
                "{call:?}"
            );
        }
    }

    #[test]
    fn user_by_screen_name_call_keeps_features_and_field_toggles() {
        let call = RawCall::UserByScreenName {
            screen_name: "jack",
        }
        .prepare(&provider())
        .unwrap();
        let keys: Vec<&str> = call.query.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["features", "fieldToggles", "variables"]);
        let variables: Value = serde_json::from_str(&call.query[2].1).unwrap();
        assert_eq!(variables["screen_name"], "jack");
        assert!(variables.get("cursor").is_none());
    }
}
