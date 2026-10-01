//! X GraphQL 端点定义（queryId / features / operation 名 / HTTP 方法）。
//!
//! # 这些常量是「组件内部实现」，绝不进契约
//!
//! `docs/01-ARCHITECTURE.md` §2：契约里一旦出现 queryId、`features` 常量、端点路径，
//! 契约就被焊死在今天的 X 上——而做这件事的全部意义就是不要被焊死。
//! `contract_files_do_not_leak_implementation_details` 测试会扫描
//! `docs/CONTRACT.md` 与 `contract/xspider.schema.json`，确保这些字面量没有漏出去。
//! 它用的是本文件里的**真实常量**，所以新加端点会自动被覆盖。
//!
//! # 移植纪律
//!
//! 权威参照顺序：`x-spider-mac/src/twitter/api.ts`（行为权威）→
//! `XSpiderMac/Sources/XSpiderMac/Services/TwitterAPI.swift`（带实测注释）。
//! **动请求构造前先逐字对齐**，不要"顺手优化"——你以为的优化通常就是 404 或空页的来源
//! （`docs/02-X-DOMAIN-NOTES.md` §0）。
//!
//! `features` 字面量刻意用 `concat!` 按原键序分段写：分行只为可读，
//! 拼接后必须与上游**逐字一致**。改它们时请对着上游 diff。
//!
//! # queryId 与 openapi.yaml 的分歧（已核查，按运行代码走）
//!
//! `x-spider-mac/.fetch/openapi.yaml` 里记录的默认 queryId 有三个与运行代码不同：
//! `UserMedia` / `UserTweets` / `TweetDetail`。以**实测能通**的运行代码为准，
//! 也就是本文件里的值。

use xspider_core::transport::HttpMethod;

/// 一个 GraphQL 端点的全部易变面。改版时就是改这里。
#[derive(Debug, Clone, Copy)]
pub(crate) struct GraphQlEndpoint {
    /// operationName，同时是 URL 的最后一段。
    pub operation: &'static str,
    /// X 的持久化查询 id。会失效，需要自愈（`docs/02` §A3）。
    pub query_id: &'static str,
    /// `features` 常量。**每个端点组合不同**，随改版变化（`docs/02` §A6）。
    pub features: &'static str,
    /// 搜索端点是 POST（`docs/02` §A2：GET 一律 404，且与 queryId 无关）。
    pub method: HttpMethod,
}

impl GraphQlEndpoint {
    /// `/i/api/graphql/<queryId>/<Operation>`。
    ///
    /// 带参数的版本给"queryId 自愈后重试"用——自愈只换 queryId，别的都不动。
    pub(crate) fn path_with(&self, query_id: &str) -> String {
        format!("/i/api/graphql/{query_id}/{}", self.operation)
    }

    pub(crate) fn path(&self) -> String {
        self.path_with(self.query_id)
    }
}

// ---------------------------------------------------------------------------
// 端点定义
// ---------------------------------------------------------------------------

/// `UserByScreenName`（上游 `getUser`）。
pub(crate) const USER_BY_SCREEN_NAME: GraphQlEndpoint = GraphQlEndpoint {
    operation: "UserByScreenName",
    query_id: "NimuplG1OB7Fd2btCLdBOw",
    method: HttpMethod::Get,
    features: concat!(
        r#"{"hidden_profile_likes_enabled":true,"hidden_profile_subscriptions_enabled":true,"#,
        r#""responsive_web_graphql_exclude_directive_enabled":true,"verified_phone_label_enabled":false,"#,
        r#""subscriptions_verification_info_is_identity_verified_enabled":true,"#,
        r#""subscriptions_verification_info_verified_since_enabled":true,"#,
        r#""highlights_tweets_tab_ui_enabled":true,"responsive_web_twitter_article_notes_tab_enabled":false,"#,
        r#""creator_subscriptions_tweet_preview_api_enabled":true,"#,
        r#""responsive_web_graphql_skip_user_profile_image_extensions_enabled":false,"#,
        r#""responsive_web_graphql_timeline_navigation_enabled":true}"#,
    ),
};

/// `fieldToggles`。**只有 `UserByScreenName` 用它**（已核对两个上游，其余端点都没有）。
pub(crate) const USER_BY_SCREEN_NAME_FIELD_TOGGLES: &str = r#"{"withAuxiliaryUserLabels":false}"#;

/// `UserMedia`（上游 `getUserMedias`）。**`Following` 复用同一份 features**。
pub(crate) const USER_MEDIA: GraphQlEndpoint = GraphQlEndpoint {
    operation: "UserMedia",
    query_id: "cEjpJXA15Ok78yO4TUQPeQ",
    method: HttpMethod::Get,
    features: concat!(
        r#"{"responsive_web_graphql_exclude_directive_enabled":true,"verified_phone_label_enabled":false,"#,
        r#""creator_subscriptions_tweet_preview_api_enabled":true,"#,
        r#""responsive_web_graphql_timeline_navigation_enabled":true,"#,
        r#""responsive_web_graphql_skip_user_profile_image_extensions_enabled":false,"#,
        r#""c9s_tweet_anatomy_moderator_badge_enabled":true,"tweetypie_unmention_optimization_enabled":true,"#,
        r#""responsive_web_edit_tweet_api_enabled":true,"#,
        r#""graphql_is_translatable_rweb_tweet_is_translatable_enabled":true,"#,
        r#""view_counts_everywhere_api_enabled":true,"longform_notetweets_consumption_enabled":true,"#,
        r#""responsive_web_twitter_article_tweet_consumption_enabled":true,"#,
        r#""tweet_awards_web_tipping_enabled":false,"freedom_of_speech_not_reach_fetch_enabled":true,"#,
        r#""standardized_nudges_misinfo":true,"#,
        r#""tweet_with_visibility_results_prefer_gql_limited_actions_policy_enabled":true,"#,
        r#""rweb_video_timestamps_enabled":true,"longform_notetweets_rich_text_read_enabled":true,"#,
        r#""longform_notetweets_inline_media_enabled":true,"#,
        r#""responsive_web_media_download_video_enabled":false,"responsive_web_enhance_cards_enabled":false}"#,
    ),
};

/// `UserTweets`（上游 `getUserTweets`）。
pub(crate) const USER_TWEETS: GraphQlEndpoint = GraphQlEndpoint {
    operation: "UserTweets",
    query_id: "9zyyd1hebl7oNWIPdA8HRw",
    method: HttpMethod::Get,
    features: concat!(
        r#"{"rweb_tipjar_consumption_enabled":true,"responsive_web_graphql_exclude_directive_enabled":true,"#,
        r#""verified_phone_label_enabled":false,"creator_subscriptions_tweet_preview_api_enabled":true,"#,
        r#""responsive_web_graphql_timeline_navigation_enabled":true,"#,
        r#""responsive_web_graphql_skip_user_profile_image_extensions_enabled":false,"#,
        r#""communities_web_enable_tweet_community_results_fetch":true,"#,
        r#""c9s_tweet_anatomy_moderator_badge_enabled":true,"articles_preview_enabled":false,"#,
        r#""tweetypie_unmention_optimization_enabled":true,"responsive_web_edit_tweet_api_enabled":true,"#,
        r#""graphql_is_translatable_rweb_tweet_is_translatable_enabled":true,"#,
        r#""view_counts_everywhere_api_enabled":true,"longform_notetweets_consumption_enabled":true,"#,
        r#""responsive_web_twitter_article_tweet_consumption_enabled":true,"#,
        r#""tweet_awards_web_tipping_enabled":false,"#,
        r#""creator_subscriptions_quote_tweet_preview_enabled":false,"#,
        r#""freedom_of_speech_not_reach_fetch_enabled":true,"standardized_nudges_misinfo":true,"#,
        r#""tweet_with_visibility_results_prefer_gql_limited_actions_policy_enabled":true,"#,
        r#""tweet_with_visibility_results_prefer_gql_media_interstitial_enabled":false,"#,
        r#""rweb_video_timestamps_enabled":true,"longform_notetweets_rich_text_read_enabled":true,"#,
        r#""longform_notetweets_inline_media_enabled":true,"responsive_web_enhance_cards_enabled":false}"#,
    ),
};

/// `TweetDetail`。**`HomeTimeline` 与 `HomeLatestTimeline` 复用同一份 features**。
pub(crate) const TWEET_DETAIL: GraphQlEndpoint = GraphQlEndpoint {
    operation: "TweetDetail",
    query_id: "XMOz5h24KAZ86qKffKTLdQ",
    method: HttpMethod::Get,
    features: concat!(
        r#"{"articles_preview_enabled":false,"c9s_tweet_anatomy_moderator_badge_enabled":true,"#,
        r#""communities_web_enable_tweet_community_results_fetch":true,"#,
        r#""creator_subscriptions_quote_tweet_preview_enabled":false,"#,
        r#""creator_subscriptions_tweet_preview_api_enabled":true,"#,
        r#""freedom_of_speech_not_reach_fetch_enabled":true,"#,
        r#""graphql_is_translatable_rweb_tweet_is_translatable_enabled":true,"#,
        r#""longform_notetweets_consumption_enabled":true,"longform_notetweets_inline_media_enabled":true,"#,
        r#""longform_notetweets_rich_text_read_enabled":true,"responsive_web_edit_tweet_api_enabled":true,"#,
        r#""responsive_web_enhance_cards_enabled":false,"#,
        r#""responsive_web_graphql_exclude_directive_enabled":true,"#,
        r#""responsive_web_graphql_skip_user_profile_image_extensions_enabled":false,"#,
        r#""responsive_web_grok_community_note_auto_translation_is_enabled":false,"#,
        r#""responsive_web_graphql_timeline_navigation_enabled":true,"#,
        r#""responsive_web_grok_imagine_annotation_enabled":false,"#,
        r#""responsive_web_media_download_video_enabled":false,"#,
        r#""responsive_web_profile_redirect_enabled":true,"#,
        r#""responsive_web_twitter_article_tweet_consumption_enabled":true,"#,
        r#""rweb_tipjar_consumption_enabled":true,"rweb_video_timestamps_enabled":true,"#,
        r#""standardized_nudges_misinfo":true,"tweet_awards_web_tipping_enabled":false,"#,
        r#""tweet_with_visibility_results_prefer_gql_limited_actions_policy_enabled":true,"#,
        r#""tweet_with_visibility_results_prefer_gql_media_interstitial_enabled":false,"#,
        r#""tweetypie_unmention_optimization_enabled":true,"verified_phone_label_enabled":false,"#,
        r#""view_counts_everywhere_api_enabled":true,"#,
        r#""responsive_web_grok_analyze_button_fetch_trends_enabled":false,"#,
        r#""premium_content_api_read_enabled":false,"#,
        r#""profile_label_improvements_pcf_label_in_post_enabled":false,"#,
        r#""responsive_web_grok_share_attachment_enabled":false,"#,
        r#""responsive_web_grok_analyze_post_followups_enabled":false,"#,
        r#""responsive_web_grok_image_annotation_enabled":false,"#,
        r#""responsive_web_grok_analysis_button_from_backend":false,"responsive_web_jetfuel_frame":false,"#,
        r#""rweb_video_screen_enabled":true,"responsive_web_grok_show_grok_translated_post":true}"#,
    ),
};

/// `SearchTimeline`。**必须 POST**（`docs/02` §A2）。
///
/// 这里的 `query_id` 是**默认值**；运行时优先用自愈拿到的当前值
/// （见 `crate::search_query_id`）。
pub(crate) const SEARCH_TIMELINE: GraphQlEndpoint = GraphQlEndpoint {
    operation: "SearchTimeline",
    query_id: "Yw6L66Pw54NHKuq4Dp7b4Q",
    method: HttpMethod::Post,
    features: concat!(
        r#"{"rweb_video_screen_enabled":true,"rweb_cashtags_enabled":true,"#,
        r#""profile_label_improvements_pcf_label_in_post_enabled":true,"#,
        r#""responsive_web_profile_redirect_enabled":false,"rweb_tipjar_consumption_enabled":false,"#,
        r#""verified_phone_label_enabled":false,"#,
        r#""responsive_web_graphql_timeline_navigation_enabled":true,"#,
        r#""creator_subscriptions_tweet_preview_api_enabled":true,"#,
        r#""responsive_web_graphql_exclude_directive_enabled":false,"#,
        r#""responsive_web_graphql_skip_user_profile_image_extensions_enabled":false,"#,
        r#""premium_content_api_read_enabled":false,"#,
        r#""communities_web_enable_tweet_community_results_fetch":true,"#,
        r#""c9s_tweet_anatomy_moderator_badge_enabled":true,"articles_preview_enabled":true,"#,
        r#""responsive_web_edit_tweet_api_enabled":true,"#,
        r#""graphql_is_translatable_rweb_tweet_is_translatable_enabled":true,"#,
        r#""view_counts_everywhere_api_enabled":true,"longform_notetweets_consumption_enabled":true,"#,
        r#""responsive_web_twitter_article_tweet_consumption_enabled":true,"#,
        r#""tweet_awards_web_tipping_enabled":false,"freedom_of_speech_not_reach_fetch_enabled":true,"#,
        r#""standardized_nudges_misinfo":true,"#,
        r#""tweet_with_visibility_results_prefer_gql_limited_actions_policy_enabled":true,"#,
        r#""longform_notetweets_rich_text_read_enabled":true,"#,
        r#""longform_notetweets_inline_media_enabled":false,"responsive_web_enhance_cards_enabled":false}"#,
    ),
};

/// `HomeTimeline`（"为你推荐"）。features 复用 [`TWEET_DETAIL`]。
pub(crate) const HOME_TIMELINE: GraphQlEndpoint = GraphQlEndpoint {
    operation: "HomeTimeline",
    query_id: "7zlnp2TxC044W4C1ZUJMHw",
    method: HttpMethod::Get,
    features: TWEET_DETAIL.features,
};

/// `HomeLatestTimeline`（"正在关注"）。features 复用 [`TWEET_DETAIL`]。
pub(crate) const HOME_LATEST_TIMELINE: GraphQlEndpoint = GraphQlEndpoint {
    operation: "HomeLatestTimeline",
    query_id: "0dateTVgvXjpkf7kyBZy0g",
    method: HttpMethod::Get,
    features: TWEET_DETAIL.features,
};

/// `Following`（关注列表）。features 复用 [`USER_MEDIA`]。
pub(crate) const FOLLOWING: GraphQlEndpoint = GraphQlEndpoint {
    operation: "Following",
    query_id: "F42cDX8PDFxkbjjq6JrM2w",
    method: HttpMethod::Get,
    features: USER_MEDIA.features,
};

/// `home_timeline` 的两种模式。**两种模式的 operationName 与 queryId 都不同**
/// （这不是笔误：`following` 模式用的是 `HomeLatestTimeline`）。
pub(crate) fn match_home_mode(mode: &str) -> xspider_core::error::XResult<GraphQlEndpoint> {
    match mode {
        "for_you" => Ok(HOME_TIMELINE),
        "following" => Ok(HOME_LATEST_TIMELINE),
        other => Err(xspider_core::error::XError::invalid_request(format!(
            "mode 必须是 \"for_you\" 或 \"following\"，收到 {other:?}"
        ))),
    }
}

/// 所有端点。测试用它做"契约里不出现这些字面量"的全量扫描。
#[cfg(test)]
pub(crate) fn all_endpoints() -> Vec<GraphQlEndpoint> {
    vec![
        USER_BY_SCREEN_NAME,
        USER_MEDIA,
        USER_TWEETS,
        TWEET_DETAIL,
        SEARCH_TIMELINE,
        HOME_TIMELINE,
        HOME_LATEST_TIMELINE,
        FOLLOWING,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn path_is_built_from_query_id_and_operation() {
        assert_eq!(
            USER_BY_SCREEN_NAME.path(),
            "/i/api/graphql/NimuplG1OB7Fd2btCLdBOw/UserByScreenName"
        );
        assert_eq!(
            USER_TWEETS.path(),
            "/i/api/graphql/9zyyd1hebl7oNWIPdA8HRw/UserTweets"
        );
        assert_eq!(
            SEARCH_TIMELINE.path(),
            "/i/api/graphql/Yw6L66Pw54NHKuq4Dp7b4Q/SearchTimeline"
        );
    }

    /// 搜索必须 POST：GET 一律 404，且与 queryId 无关（docs/02 §A2）。
    #[test]
    fn only_search_uses_post() {
        for ep in all_endpoints() {
            let expected = if ep.operation == "SearchTimeline" {
                HttpMethod::Post
            } else {
                HttpMethod::Get
            };
            assert_eq!(ep.method, expected, "{} 的方法不对", ep.operation);
        }
    }

    #[test]
    fn every_features_literal_is_valid_json_of_booleans() {
        for ep in all_endpoints() {
            let v: serde_json::Value = serde_json::from_str(ep.features)
                .unwrap_or_else(|e| panic!("{} 的 features 不是合法 JSON：{e}", ep.operation));
            let obj = v
                .as_object()
                .unwrap_or_else(|| panic!("{} 的 features 不是对象", ep.operation));
            assert!(obj.len() > 10, "{} 的 features 条目太少", ep.operation);
            for (k, val) in obj {
                assert!(
                    val.is_boolean(),
                    "{} 的 feature {k} 不是布尔值（X 的 features 全是布尔）",
                    ep.operation
                );
            }
        }
    }

    #[test]
    fn user_by_screen_name_features_keeps_the_upstream_key_set() {
        let v: serde_json::Value = serde_json::from_str(USER_BY_SCREEN_NAME.features).unwrap();
        let keys: BTreeSet<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys.len(), 11);
        assert!(keys.contains("verified_phone_label_enabled"));
        assert!(keys.contains("hidden_profile_subscriptions_enabled"));
    }

    #[test]
    fn field_toggles_is_valid_json() {
        let v: serde_json::Value = serde_json::from_str(USER_BY_SCREEN_NAME_FIELD_TOGGLES).unwrap();
        assert_eq!(v["withAuxiliaryUserLabels"], false);
    }

    #[test]
    fn shared_features_are_shared_by_identity_not_by_copy() {
        // HomeTimeline / HomeLatestTimeline / TweetDetail 必须真的是同一份字面量，
        // 否则将来只改一处就会漏改（这是 docs/02 §A6 "每个端点组合不同" 的反面守卫）
        assert_eq!(HOME_TIMELINE.features, TWEET_DETAIL.features);
        assert_eq!(HOME_LATEST_TIMELINE.features, TWEET_DETAIL.features);
        assert_eq!(FOLLOWING.features, USER_MEDIA.features);
        // 但不应与其它端点相同（相同说明复制错了）
        assert_ne!(USER_MEDIA.features, USER_TWEETS.features);
        assert_ne!(USER_MEDIA.features, TWEET_DETAIL.features);
        assert_ne!(USER_TWEETS.features, TWEET_DETAIL.features);
        assert_ne!(SEARCH_TIMELINE.features, USER_TWEETS.features);
    }

    #[test]
    fn home_modes_map_to_different_endpoints() {
        let for_you = match_home_mode("for_you").unwrap();
        let following = match_home_mode("following").unwrap();
        assert_eq!(for_you.operation, "HomeTimeline");
        assert_eq!(following.operation, "HomeLatestTimeline");
        assert_ne!(for_you.query_id, following.query_id);
        // 两者共用同一份 features
        assert_eq!(for_you.features, following.features);

        let err = match_home_mode("nope").unwrap_err();
        assert_eq!(err.code(), xspider_core::error::ErrorCode::InvalidRequest);
        assert!(err.to_string().contains("for_you"), "{err}");
    }

    #[test]
    fn path_with_swaps_only_the_query_id() {
        assert_eq!(
            SEARCH_TIMELINE.path_with("auLkqtmHqYEpRvflfvLhyQ"),
            "/i/api/graphql/auLkqtmHqYEpRvflfvLhyQ/SearchTimeline"
        );
    }

    /// **契约不得泄露实现细节**（`docs/01-ARCHITECTURE.md` §2、`AGENTS.md` 铁律 2）。
    ///
    /// 拿本文件里的真实常量去扫描文档与 schema：一旦有人"顺手"把 queryId 或 features
    /// 写进契约，它会立刻红。用真实常量而不是硬编码字面量，新端点会自动被覆盖。
    #[test]
    fn contract_files_do_not_leak_implementation_details() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut forbidden: Vec<String> = vec!["/i/api/".to_string()];
        for ep in all_endpoints() {
            forbidden.push(ep.query_id.to_string());
            forbidden.push(format!("{}/{}", ep.query_id, ep.operation));
            forbidden.push(format!("/{}", ep.operation));
            let features: serde_json::Value = serde_json::from_str(ep.features).unwrap();
            for key in features.as_object().unwrap().keys().take(3) {
                forbidden.push(key.clone());
            }
        }

        for rel in ["docs/CONTRACT.md", "contract/xspider.schema.json"] {
            let path = root.join(rel);
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("读取 {} 失败：{e}", path.display()));
            for needle in &forbidden {
                assert!(
                    !text.contains(needle.as_str()),
                    "契约文件 {rel} 里出现了实现细节 {needle:?}——\
                     契约一旦包含 queryId / features / 端点路径，就被焊死在今天的 X 上了。"
                );
            }
        }
    }

    /// HTTP 头名同样不许进契约（凭据与传输细节都不是契约的事）。
    #[test]
    fn contract_files_do_not_leak_header_names() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let headers = [
            "x-client-transaction-id",
            "X-Csrf-Token",
            "x-twitter-active-user",
            "x-twitter-client-language",
            "Authorization: Bearer",
        ];
        for rel in ["docs/CONTRACT.md", "contract/xspider.schema.json"] {
            let text = std::fs::read_to_string(root.join(rel)).unwrap();
            for needle in headers {
                assert!(
                    !text.contains(needle),
                    "契约文件 {rel} 里出现了 HTTP 头 {needle:?}"
                );
            }
        }
    }
}
