//! # xspider-fetch
//!
//! 在线数据获取组件：用户、媒体时间线、推文时间线、推文详情树、搜索、主页时间线、
//! 关注列表、互动（点赞/转推/书签）。
//!
//! ## 与内核的分工
//!
//! HTTP、凭据、限流闸门与 429 熔断、签名、错误分类都在 [`xspider_core`]；
//! 本组件只放**X 的领域知识**：端点定义（queryId / features）、请求变量的构造、
//! 响应解析与 DTO 映射。
//!
//! ## 分页：只给「单页 + 游标」
//!
//! 不提供自动翻页的流（`docs/01-ARCHITECTURE.md` §3 硬性设计 1）：
//! 翻页时机、过滤、去重是业务语义，必须让调用方看得见游标。
//!
//! ## 已实现的端点
//!
//! | method | 说明 |
//! |---|---|
//! | [`FetchClient::get_user`] | 按用户名取用户（M0 垂直切片） |
//! | [`FetchClient::user_medias`] | 媒体时间线 |
//! | [`FetchClient::user_tweets`] | 推文时间线 |
//! | [`FetchClient::tweet_detail`] | 推文详情（focal + 回复树） |
//! | [`FetchClient::search_timeline`] | 搜索时间线（**POST**） |
//! | [`FetchClient::home_timeline`] | 主页时间线（两种模式） |
//! | [`FetchClient::following`] | 关注列表 |
//!
//! 所有分页端点都返回 [`xspider_core::paging::Page`]：**单页 + 游标，不自动翻页**。

mod calls;
mod endpoints;
mod post;
mod search;
pub mod search_query_id;
mod timeline;
mod tweet_detail;
mod user;

pub use calls::{
    RawCall, ENDPOINT_FOLLOWING, ENDPOINT_HOME_TIMELINE, ENDPOINT_SEARCH_TIMELINE,
    ENDPOINT_TWEET_DETAIL, ENDPOINT_USER_MEDIAS, ENDPOINT_USER_TWEETS,
};
pub use post::{parse_post, Media, MediaKind, MediaVariant, Post, PostAuthor};
pub use search_query_id::{extract_query_id, SearchQueryIdProvider};
pub use tweet_detail::TweetDetail;
pub use user::{
    normalize_screen_name, parse_user, parse_user_in_result, FetchClient, User,
    ENDPOINT as GET_USER_ENDPOINT,
};
