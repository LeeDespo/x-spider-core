//! `fetch.get_user`：按 `screen_name` 取用户。
//!
//! 这是 M0 垂直切片选中的那一个端点（见 `docs/ROADMAP.md` 的 M0 节）。选它的理由：
//! 它同时覆盖了**签名**（需要 `x-client-transaction-id`）、**凭据**（cookie + ct0）、
//! **GraphQL GET + URL 编码**、**DTO 映射**、以及**"不存在"这类结构化错误**，
//! 但没有分页——分页语义留在 M1 随 `user_medias` 一起做。
//!
//! 上游对照：`x-spider-mac/src/twitter/api.ts` 的 `getUser`。

use serde::{Deserialize, Serialize};

use xspider_core::cancel::CancelToken;
use xspider_core::error::{XError, XResult};
use xspider_core::stack::HttpStack;
use xspider_core::xdate::parse_x_created_at;

/// 逻辑端点名。错误与日志里用它，**不用 URL 路径**（契约里不许出现端点路径）。
pub const ENDPOINT: &str = "user_by_screen_name";

/// 用户 DTO（契约里的 `user` 对象）。
///
/// 字段命名与含义属于**对外契约**（`docs/CONTRACT.md`）：只增不改不删。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    /// 数字 id，字符串形式（X 的 id 超出 f64 精度，上游也用字符串）。
    pub id: String,
    /// 用户名（不含 `@`）。
    pub screen_name: String,
    /// 昵称（可能含任意 Unicode）。
    pub name: String,
    /// 头像 URL。
    pub avatar: String,
    /// 媒体数。**可能缺失**（X 不保证返回），缺失时是 `null` 而不是 0——
    /// 0 是"确实没有媒体"，两者语义不同。
    pub media_count: Option<u64>,
    /// 注册时间，RFC3339 UTC（由 X 的 `Wed Sep 30 12:34:56 +0000 2009` 归一化而来）。
    pub register_time: Option<String>,
}

pub struct FetchClient {
    stack: HttpStack,
    /// 搜索端点的 queryId 可能失效，需要自愈（`docs/02` §A3）。
    search_query_ids: crate::search_query_id::SearchQueryIdProvider,
    /// "我是谁"的进程内缓存：`is_following` 每条都要用它，而它要抓一次首页。
    /// **换 cookie 时必须清**（`invalidate_account_cache`），否则会拿旧账号的身份去查。
    account: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

impl FetchClient {
    pub fn new(stack: HttpStack) -> Self {
        Self {
            stack,
            search_query_ids: crate::search_query_id::SearchQueryIdProvider::new(),
            account: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }

    pub(crate) fn account_cache(&self) -> &std::sync::Mutex<Option<String>> {
        &self.account
    }

    pub fn stack(&self) -> &HttpStack {
        &self.stack
    }

    pub(crate) fn search_query_ids(&self) -> &crate::search_query_id::SearchQueryIdProvider {
        &self.search_query_ids
    }

    /// 取用户。
    ///
    /// 注意 `variables` 里**没有** cursor 键——这个端点本来就不分页；
    /// 分页端点必须遵守 `docs/02-X-DOMAIN-NOTES.md` §A1（缺省时整个键省略，不是 `null`）。
    pub async fn get_user(&self, screen_name: &str, cancel: &CancelToken) -> XResult<User> {
        let body = self
            .call_json(
                crate::calls::RawCall::UserByScreenName { screen_name },
                cancel,
            )
            .await?;
        parse_user(&body).map_err(|e| e.with_endpoint(ENDPOINT))
    }

    /// 取**原始响应**，不解析。
    ///
    /// 用途有二，都不是"顺手加的"：
    /// 1. 录制 fixture（`docs/04` §2.1 要求 fixture 必须是真实抓到的响应）；
    /// 2. canary 红了的时候，把原始响应直接存成 `field_changed_<日期>.json` 再修解析。
    pub async fn get_user_raw(
        &self,
        screen_name: &str,
        cancel: &CancelToken,
    ) -> XResult<xspider_core::transport::HttpResponse> {
        self.raw(
            crate::calls::RawCall::UserByScreenName { screen_name },
            cancel,
        )
        .await
    }

    /// **不带凭据**地发同一次请求。
    ///
    /// 用途：探测与录制。外壳在登录前需要知道"这个端点通不通"，
    /// 以及"未登录时上游到底返回什么状态码"——后者决定了它该不该提示重新登录。
    /// 带上这条路径，"未授权"这个错误分支才有真实响应可钉
    /// （而不是拿一个手写 JSON 自我满足）。
    pub async fn get_user_anonymous(
        &self,
        screen_name: &str,
        cancel: &CancelToken,
    ) -> XResult<xspider_core::transport::HttpResponse> {
        let prepared = crate::calls::RawCall::UserByScreenName { screen_name }
            .prepare(self.search_query_ids())?;
        // 用 probe_raw 而不是 raw：未登录时上游返回 403，
        // 而 `raw`（=> send）会把它变成错误——那样就看不到响应体了，
        // 而"未登录到底返回什么"正是这条路径要回答的问题。
        self.stack()
            .probe_raw(
                prepared.endpoint,
                prepared.method,
                &prepared.path,
                &prepared.query,
                false,
                cancel,
            )
            .await
    }
}

/// 归一化用户名：容忍外壳把 `@jack` 直接传进来（X 的 `variables` 里不能带 `@`）。
pub fn normalize_screen_name(raw: &str) -> XResult<String> {
    let trimmed = raw.trim().trim_start_matches('@').trim();
    if trimmed.is_empty() {
        return Err(XError::invalid_request("screen_name 不能为空"));
    }
    Ok(trimmed.to_string())
}

/// 解析 `{"data":{"user":{"result":{...}}}}` 里的用户，并给出**结构化**的错误分类。
///
/// 上游语义（`getUser`）：`data.user.result.legacy` 取不到就抛"找不到该用户"。
/// 这里把它变成 `not_found`，**而不是**让调用方去匹配文案。
pub fn parse_user(body: &serde_json::Value) -> XResult<User> {
    let result = match body
        .get("data")
        .and_then(|d| d.get("user"))
        .and_then(|u| u.get("result"))
    {
        Some(r) => r,
        // 有错误放进了 errors[] 的版本。分类规则见 not_found_or_parse_error。
        None => return Err(not_found_or_parse_error(body)),
    };
    build_user_strict(result)
}

/// 严格版：字段缺失会给出**带字段路径**的 `parse` 错误（用于单用户查询）。
///
/// **同时支持两种用户结构**（实测同一批端点两种并存，`crate::post` 里有路径常量）：
/// - 老结构：`legacy.screen_name` / `legacy.name` / `legacy.profile_image_url_https`；
/// - 新结构：`core.screen_name` / `core.name` / `avatar.image_url`，
///   且**可能整个没有 `legacy`**（TweetDetail / Following / 搜索都是这个形状）。
///
/// 只认一种的后果不是"少几个字段"，而是**整页解析失败**——那是最难查的一类。
fn build_user_strict(result: &serde_json::Value) -> XResult<User> {
    use crate::post::{
        user_field, USER_AVATAR_PATHS, USER_CREATED_AT_PATHS, USER_ID_PATHS, USER_NAME_PATHS,
        USER_SCREEN_NAME_PATHS,
    };

    let result = crate::post::unwrap_visibility(result);

    if result.get("__typename").and_then(|v| v.as_str()) == Some("UserUnavailable") {
        return Err(XError::not_found(
            "该用户当前不可用（不存在、被冻结或被屏蔽）",
        ));
    }

    let id = user_field(result, USER_ID_PATHS).ok_or_else(|| {
        XError::parse(
            "user_by_screen_name.data.user.result.rest_id",
            "既没有 rest_id 也没有 id；X 可能改版了",
        )
    })?;

    let screen_name = user_field(result, USER_SCREEN_NAME_PATHS).ok_or_else(|| {
        XError::parse(
            "user_by_screen_name.data.user.result.(legacy|core).screen_name",
            format!(
                "两种用户结构里都没有 screen_name（__typename={}）；X 可能改版了",
                result
                    .get("__typename")
                    .and_then(|v| v.as_str())
                    .unwrap_or("<无>")
            ),
        )
    })?;

    let legacy = result.get("legacy");
    Ok(User {
        id,
        screen_name,
        name: user_field(result, USER_NAME_PATHS).unwrap_or_default(),
        // 头像归一化（`https:` + `_bigger`）与参考实现一致，见 `post::normalize_avatar`
        avatar: crate::post::normalize_avatar(
            &user_field(result, USER_AVATAR_PATHS).unwrap_or_default(),
        ),
        // 媒体数：老结构在 `legacy.media_count`，新结构在 `core.tweet_counts.media_tweets`
        // ——只认一种就会在新结构上把"有内容的账号"显示成"没有媒体"
        // （参考实现也是两条路都读）。
        media_count: legacy
            .and_then(|l| l.get("media_count"))
            .and_then(|v| v.as_u64())
            .or_else(|| {
                result
                    .get("core")
                    .and_then(|c| c.get("tweet_counts"))
                    .and_then(|t| t.get("media_tweets"))
                    .and_then(|v| v.as_u64())
            }),
        register_time: user_field(result, USER_CREATED_AT_PATHS)
            .and_then(|raw| parse_x_created_at(&raw)),
    })
}

/// 宽松版：拿不到就返回 `None`（用于**列表**里的单个用户）。
///
/// 列表里某一条坏了不该让整页失败——那会把"一条数据异常"放大成"整页不可用"。
/// 但也不能静默：调用方会统计 `None` 的条数，全废时报 `parse`。
pub fn parse_user_in_result(result: &serde_json::Value) -> Option<User> {
    build_user_strict(result).ok()
}

/// X GraphQL 的结构化错误码（`errors[].code`，数字枚举，**不是文案**）。
///
/// 不要改成匹配 `message`：文案是 X 的实现细节，改版即变，而铁律明确禁止
/// 用文案做分类（`AGENTS.md`「禁止做的事」）。码值由真实 fixture 钉住
/// （见 `classification_uses_codes_not_message_text`）。
mod x_error_code {
    /// 用户不存在。
    pub const USER_NOT_FOUND: i64 = 50;
    /// 用户被冻结/不可用。
    pub const USER_SUSPENDED: i64 = 63;
}

/// 区分"用户不存在"和"我们没看懂响应"。判据全部是**结构**，不是文案。
///
/// 真实观测（2026-10-01，见 `fixtures/user_by_screen_name/not_found.json`）：
/// **不存在的 screen_name 返回 HTTP 200 + `{"data":{}}`**——既没有 `user`，
/// 也没有 `errors[]`。所以"有 `data` 对象但里面没有 `user`"就是上游在说"找不到该用户"，
/// 与上游 `getUser`（取不到 `legacy` 即抛"找不到该用户"）语义一致。
///
/// 代价要认：如果 X 哪天把 `user` 这个键**改名**了，这里会误报成 `not_found` 而不是 `parse`。
/// 兜底是**正向路径的 canary**（对真实用户断言解析成功）——改名会让正向用例立刻红。
fn not_found_or_parse_error(body: &serde_json::Value) -> XError {
    if let Some(list) = body.get("errors").and_then(|e| e.as_array()) {
        if !list.is_empty() {
            let codes: Vec<i64> = list
                .iter()
                .filter_map(|e| e.get("code").and_then(|c| c.as_i64()))
                .collect();
            // 原始消息只作为**诊断信息**带出去，不参与分类
            let messages: Vec<String> = list
                .iter()
                .filter_map(|e| e.get("message").and_then(|m| m.as_str()))
                .map(str::to_string)
                .collect();
            let joined = messages.join("；");
            if codes.contains(&x_error_code::USER_NOT_FOUND)
                || codes.contains(&x_error_code::USER_SUSPENDED)
            {
                return XError::not_found(format!("找不到该用户（上游 code {codes:?}）"));
            }
            return XError::upstream(
                200,
                format!("上游在 HTTP 200 里返回了错误（code {codes:?}）：{joined}"),
            );
        }
    }

    if let Some(data) = body.get("data").filter(|d| d.is_object()) {
        let keys: Vec<String> = data
            .as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default();
        return XError::not_found(format!(
            "找不到该用户（上游 data 里没有 user，data 的键：{keys:?}）"
        ));
    }

    XError::parse(
        "user_by_screen_name.data.user.result",
        format!(
            "响应里既没有 data 对象也没有 errors[]；顶层键：{:?}",
            body.as_object()
                .map(|o| o.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_at_signs_and_whitespace() {
        assert_eq!(normalize_screen_name("  @jack  ").unwrap(), "jack");
        assert_eq!(normalize_screen_name("jack").unwrap(), "jack");
        assert!(normalize_screen_name("@").is_err());
        assert!(normalize_screen_name("   ").is_err());
    }

    /// 请求构造本身（`features` / `fieldToggles` / `variables` 的键与键序、
    /// **cursor 缺省时整个键省略**）在 `calls.rs` 的测试里覆盖，
    /// 因为那里是唯一的构造实现点。
    #[test]
    fn user_query_construction_is_owned_by_calls_module() {
        let call = crate::calls::RawCall::UserByScreenName {
            screen_name: "jack",
        }
        .prepare(&crate::search_query_id::SearchQueryIdProvider::new())
        .unwrap();
        let keys: Vec<&str> = call.query.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["features", "fieldToggles", "variables"]);
        let variables: serde_json::Value = serde_json::from_str(&call.query[2].1).unwrap();
        assert!(variables.get("cursor").is_none(), "首页不能带 cursor");
    }

    fn fixture(name: &str) -> serde_json::Value {
        let path = format!(
            "{}/../../fixtures/user_by_screen_name/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("读取 fixture {path} 失败：{e}"));
        let envelope: serde_json::Value =
            serde_json::from_str(&text).expect("fixture 不是合法 JSON");
        envelope["response"]["body"].clone()
    }

    /// 铁律：**没有真实响应 fixture 就不许声称功能完成**（`docs/04` §2.1）。
    /// 这条测试跑的是录制下来的真实响应，不是手写样本。
    #[test]
    fn parses_real_normal_response() {
        let user = parse_user(&fixture("normal.json")).expect("真实响应必须能解析");
        assert!(!user.id.is_empty(), "rest_id 不该为空");
        assert!(!user.screen_name.is_empty());
        assert!(!user.name.is_empty());
        assert!(
            user.avatar.starts_with("https://"),
            "avatar={}",
            user.avatar
        );
        // 注册时间必须被归一化成 RFC3339 UTC
        let rt = user
            .register_time
            .as_deref()
            .expect("真实响应里有 created_at");
        assert!(rt.len() == 20 && rt.ends_with('Z'), "register_time={rt}");
        assert!(rt.starts_with("20") || rt.starts_with("19"), "{rt}");
        // 数字 id 不该被当成浮点读进来
        assert!(
            user.id.chars().all(|c| c.is_ascii_digit()),
            "id={}",
            user.id
        );
    }

    #[test]
    fn real_normal_response_is_fully_redacted() {
        // 脱敏检查：fixture 里不许残留真实个人数据（docs/04 §2.1）
        let text = serde_json::to_string(&fixture("normal.json")).unwrap();
        for forbidden in ["auth_token", "ct0", "twid="] {
            assert!(
                !text.contains(forbidden),
                "fixture 里出现了凭据痕迹：{forbidden}"
            );
        }
    }

    #[test]
    fn real_not_found_response_maps_to_not_found() {
        let err = parse_user(&fixture("not_found.json")).expect_err("不存在的用户必须报错");
        assert_eq!(
            err.code(),
            xspider_core::error::ErrorCode::NotFound,
            "实际：{err}"
        );
        // 错误必须带端点上下文，否则线上只看得到"解析失败"
        assert_eq!(err.with_endpoint(ENDPOINT).endpoint(), Some(ENDPOINT));
    }

    /// 真实观测：不存在的 screen_name → HTTP 200 + `{"data":{}}`（见 not_found.json）。
    /// 结构里没有 `user` 就是上游在说"找不到该用户"。
    #[test]
    fn empty_data_object_means_not_found() {
        let err = parse_user(&serde_json::json!({ "data": {} })).unwrap_err();
        assert_eq!(err.code(), xspider_core::error::ErrorCode::NotFound);
    }

    /// 分类只看 `errors[].code`，**不看 message 文案**。
    ///
    /// 这条测试是"禁止用错误文案做判断"这条铁律的守卫：
    /// 给一个文案说"not found"但 code 不认识 → 必须是 upstream；
    /// 给一个 code 认识但文案乱七八糟 → 必须是 not_found。
    #[test]
    fn classification_uses_codes_not_message_text() {
        let misleading_text = serde_json::json!({
            "errors": [{ "code": 9999, "message": "Sorry, that user was not found." }]
        });
        assert_eq!(
            parse_user(&misleading_text).unwrap_err().code(),
            xspider_core::error::ErrorCode::Upstream,
            "文案不能影响分类"
        );

        let misleading_code = serde_json::json!({
            "errors": [{ "code": 50, "message": "aaaaaaaa-bbbbbb-cccccc" }]
        });
        assert_eq!(
            parse_user(&misleading_code).unwrap_err().code(),
            xspider_core::error::ErrorCode::NotFound,
            "code 说了算"
        );
    }

    #[test]
    fn suspended_user_maps_to_not_found() {
        let body = serde_json::json!({
            "errors": [{ "code": 63, "message": "User has been suspended" }]
        });
        assert_eq!(
            parse_user(&body).unwrap_err().code(),
            xspider_core::error::ErrorCode::NotFound
        );
    }

    #[test]
    fn user_unavailable_maps_to_not_found() {
        let body = serde_json::json!({
            "data": { "user": { "result": { "__typename": "UserUnavailable", "reason": "Suspended" } } }
        });
        let err = parse_user(&body).unwrap_err();
        assert_eq!(err.code(), xspider_core::error::ErrorCode::NotFound);
    }

    #[test]
    fn missing_legacy_is_a_parse_error_with_field_context() {
        // 这条覆盖"字段改名/消失"这一类改版：必须报 parse，且 context 指向具体字段
        let body = serde_json::json!({
            "data": { "user": { "result": { "__typename": "User", "rest_id": "1" } } }
        });
        let err = parse_user(&body).unwrap_err();
        assert_eq!(err.code(), xspider_core::error::ErrorCode::Parse);
        let v = xspider_core::err_envelope(&err);
        assert!(
            v["error"]["context"].as_str().unwrap().contains("legacy"),
            "{v}"
        );
    }

    #[test]
    fn missing_screen_name_is_a_parse_error_naming_the_field() {
        let body = serde_json::json!({
            "data": { "user": { "result": {
                "rest_id": "1", "legacy": { "name": "n", "profile_image_url_https": "https://a/b.jpg" }
            } } }
        });
        let err = parse_user(&body).unwrap_err();
        assert_eq!(err.code(), xspider_core::error::ErrorCode::Parse);
        assert!(
            err.to_string().contains("screen_name"),
            "错误文案要能指出缺哪个字段：{err}"
        );
    }

    #[test]
    fn no_data_no_errors_is_a_parse_error_listing_top_level_keys() {
        let body = serde_json::json!({ "unexpected": true });
        let err = parse_user(&body).unwrap_err();
        assert_eq!(err.code(), xspider_core::error::ErrorCode::Parse);
        assert!(err.to_string().contains("unexpected"), "{err}");
    }

    #[test]
    fn missing_media_count_stays_none_not_zero() {
        let body = serde_json::json!({
            "data": { "user": { "result": {
                "rest_id": "1",
                "legacy": { "screen_name": "s", "name": "n", "profile_image_url_https": "https://a/b.jpg" }
            } } }
        });
        let user = parse_user(&body).unwrap();
        assert_eq!(user.media_count, None, "缺失不等于 0");
        assert_eq!(user.register_time, None);
    }

    #[test]
    fn upstream_error_messages_are_surfaced_not_swallowed() {
        let body = serde_json::json!({
            "errors": [{ "message": "Something brand new broke" }]
        });
        let err = parse_user(&body).unwrap_err();
        assert_eq!(err.code(), xspider_core::error::ErrorCode::Upstream);
        assert!(
            err.to_string().contains("Something brand new broke"),
            "{err}"
        );
    }
}
