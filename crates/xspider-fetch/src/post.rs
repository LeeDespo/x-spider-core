//! 推文与媒体的 DTO，以及"从一个 `result` 解析成 Post"。
//!
//! 上游对照：`x-spider-mac/src/twitter/api.ts` 的 `mapTwitterPosts` /
//! `mapTwitterMedias`；层级与字段名尽量与其一致，便于逐字核对。
//!
//! 契约字段命名属于对外契约（`docs/CONTRACT.md`）：**只增不改不删**。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use xspider_core::xdate::parse_x_created_at;

/// 媒体类型。沿用 X 自己的命名，避免我们再发明一套词汇。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Photo,
    Video,
    /// X 的 `animated_gif`（实际是 mp4，没有音频）。
    AnimatedGif,
}

impl MediaKind {
    fn from_x(raw: &str) -> Option<Self> {
        match raw {
            "photo" => Some(MediaKind::Photo),
            "video" => Some(MediaKind::Video),
            "animated_gif" => Some(MediaKind::AnimatedGif),
            _ => None,
        }
    }
}

/// 一个视频/动图的码率变体。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaVariant {
    pub url: String,
    pub content_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitrate: Option<u64>,
}

/// 一个可下载的媒体。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Media {
    pub kind: MediaKind,
    /// X 的 media id（字符串）。它比 URL 稳定，适合做去重与文件名的一部分。
    pub id: String,
    /// **可直接下载的 URL**。
    ///
    /// - 图片：`media_url_https`（不含 `?name=`，见下）；
    /// - 视频/动图：已按 `docs/02` §C7 选好——**过滤掉 HLS（`application/x-mpegURL`）
    ///   后取最高码率**的变体。
    pub url: String,
    /// 扩展名（不含点），由 URL 推断。外壳拼文件名时会用到。
    pub ext: String,
    /// **封面图**（X 的 `media_url_https`）：视频/动图在播放前显示的静帧，
    /// 图片则是它本身（不带 `?name=`）。
    ///
    /// **它不是可下载的媒体文件**——视频的播放/下载地址在 [`Self::url`] 与
    /// [`Self::variants`] 里。两者混淆的后果很具体：界面上把 mp4 当图片解码，
    /// 视频格子整片空白（参考实现的 `TwitterMedia.url` 一直就是这个封面语义，
    /// 抽取时漏了这个字段，直到真接进外壳才暴露）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poster_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aspect_ratio: Option<[u64; 2]>,
    /// 全部码率变体（已过滤 HLS）。图片为空数组。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<MediaVariant>,
}

impl Media {
    /// 缩略图用的小图 URL。
    ///
    /// `docs/02` §C6：`?name=small` 是 **680px 宽**（不是 120px），
    /// 且缩略图要按目标尺寸**降采样解码**，别全尺寸解码后缩放（网格滑动会卡）。
    /// 我们只提供 URL，解码策略是外壳的事。
    pub fn thumbnail_url(&self) -> String {
        Self::with_name(&self.url, "small")
    }

    /// 原图 URL（`?name=orig`）。仅对图片有意义。
    pub fn original_url(&self) -> String {
        Self::with_name(&self.url, "orig")
    }

    fn with_name(url: &str, name: &str) -> String {
        let base = url.split('?').next().unwrap_or(url);
        format!("{base}?name={name}")
    }
}

/// 推文作者（比用户 DTO 精简：列表里只需要能展示与链接的东西）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostAuthor {
    pub id: String,
    pub screen_name: String,
    pub name: String,
    pub avatar: String,
}

/// 一条推文。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Post {
    pub id: String,
    /// RFC3339 UTC；X 没给 `created_at` 时为 `None`（**不丢条目**，见 docs/02 §D4）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    pub full_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub views: Option<u64>,
    pub favorite_count: u64,
    pub retweet_count: u64,
    pub reply_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bookmark_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quote_count: Option<u64>,
    pub possibly_sensitive: bool,
    /// 我有没有点赞 / 转推 / 收藏这条（X 在 `legacy` 里给的就是这三面旗）。
    ///
    /// **不是"计数"，是"状态"**：外壳靠它决定按钮是实心还是空心——
    /// 参考实现的 `mapTwitterPost` 读的也是这三个（`legacy.favorited` 等）。
    /// 缺了它们，接入后点赞/收藏按钮会全部显示成"没点过"。
    pub favorited: bool,
    pub retweeted: bool,
    pub bookmarked: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub medias: Vec<Media>,
    pub author: PostAuthor,
    /// 话题标签文本（不含 `#`）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// 引用/转推关系。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quoted_id: Option<String>,
    /// 「本条回复的是谁」——必须用**被回复者**，不是本条作者（`docs/02` §C2）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_reply_to_screen_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_reply_to_id: Option<String>,
    /// 转推时填"是谁转的"（`include_retweets` 打开时才有）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retweeted_by: Option<PostAuthor>,
}

impl Post {
    pub fn media_count(&self) -> usize {
        self.medias.len()
    }

    /// 「有没有 media」——`require_media` 筛选用。
    pub fn has_media(&self) -> bool {
        !self.medias.is_empty()
    }
}

/// 解析一个 `tweet_results.result`。**返回 `None` 表示"这条该跳过"**，不是错误：
/// 见到 `TweetUnavailable`、或结构变化导致认不出来，都走这条路，
/// 由调用方按"整页候选是否全废"来决定要不要报 `parse`。
pub fn parse_post(result: &Value) -> Option<Post> {
    let result = unwrap_visibility(result);
    let id = result.get("rest_id")?.as_str()?.to_string();

    // 被删/被冻结/不可见的推文：跳过，不是错误
    if let Some(typename) = result.get("__typename").and_then(|v| v.as_str()) {
        if typename == "TweetUnavailable" {
            return None;
        }
    }

    let legacy = result.get("legacy")?;
    let author = parse_author(result)?;

    let medias = legacy
        .get("entities")
        .and_then(|e| e.get("media"))
        .and_then(|m| m.as_array())
        .map(|arr| arr.iter().filter_map(parse_media).collect())
        .unwrap_or_default();

    let tags = legacy
        .get("entities")
        .and_then(|e| e.get("hashtags"))
        .and_then(|h| h.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.get("text").and_then(|v| v.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    Some(Post {
        id,
        created_at: nested_str(legacy, &[&["created_at"]])
            .or_else(|| nested_str(result, USER_CREATED_AT_PATHS))
            .and_then(parse_x_created_at),
        full_text: full_text_of(result, legacy),
        lang: str_field(legacy, "lang"),
        views: result
            .get("views")
            .and_then(|v| v.get("count"))
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok()),
        favorite_count: u64_field(legacy, "favorite_count"),
        retweet_count: u64_field(legacy, "retweet_count"),
        reply_count: u64_field(legacy, "reply_count"),
        bookmark_count: opt_u64_field(legacy, "bookmark_count"),
        quote_count: opt_u64_field(legacy, "quote_count"),
        possibly_sensitive: bool_field(legacy, "possibly_sensitive"),
        favorited: bool_field(legacy, "favorited"),
        retweeted: bool_field(legacy, "retweeted"),
        bookmarked: bool_field(legacy, "bookmarked"),
        medias,
        author,
        tags,
        quoted_id: result
            .get("quoted_status_result")
            .and_then(|q| q.get("result"))
            .map(unwrap_visibility)
            .and_then(|r| r.get("rest_id"))
            .and_then(|v| v.as_str())
            .map(str::to_string),
        in_reply_to_screen_name: str_field(legacy, "in_reply_to_screen_name"),
        in_reply_to_id: str_field(legacy, "in_reply_to_status_id_str"),
        retweeted_by: None,
    })
}

/// 转推包装：把 `legacy.retweeted_status_result` 里的原推提上来，并记下转推者。
///
/// 注意 `retweeted_status_result` 里可能是 `{"result": ...}` 也可能是 `{"tweet": ...}`
/// ——两种都在真实响应里出现过（上游 Swift 也是两个都认）。
pub fn unwrap_retweet(result: &Value) -> Option<(Post, Option<PostAuthor>)> {
    let wrapper = result
        .get("legacy")
        .and_then(|l| l.get("retweeted_status_result"))?;
    let inner = wrapper.get("result").or_else(|| wrapper.get("tweet"))?;
    let post = parse_post(inner)?;
    let retweeter = parse_author(unwrap_visibility(result));
    Some((post, retweeter))
}

/// `TweetWithVisibilityResults` 拆一层（上游两个实现都这么做）。
pub fn unwrap_visibility(result: &Value) -> &Value {
    if result.get("__typename").and_then(|v| v.as_str()) == Some("TweetWithVisibilityResults")
        || result.get("tweet").is_some() && result.get("rest_id").is_none()
    {
        if let Some(tweet) = result.get("tweet") {
            return tweet;
        }
    }
    result
}

/// 按候选路径取值（首个命中的赢）。
///
/// 需要的理由：**同一个端点在不同响应用了两种用户结构**（实测两种并存）——
/// 老结构把字段放在 `legacy` 里，新结构放在 `core` 里，头像在 `avatar.image_url`。
/// 只认一种就会让另一半响应整页解析失败（这正是"J1 实测"记下的那个坑）。
pub(crate) fn nested_str<'a>(obj: &'a Value, paths: &[&[&str]]) -> Option<&'a str> {
    for path in paths {
        let mut node = obj;
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
            if let Some(text) = node.as_str() {
                return Some(text);
            }
        }
    }
    None
}

/// 上一结构里的头像尺寸后缀换成 `_bigger`，并把 `//` 补成 `https://`。
///
/// 与参考实现一致（`x-spider-mac/.../TwitterAPI.swift` 的 `mapTwitterUser`）：
/// 列表里显示的是 `_normal`（48px）会糊，参考实现用 `_bigger`（73px）。
/// **组件与外壳给同一个值**，接入时外壳那行 `replacingOccurrences(of:)` 就可以删掉。
pub(crate) fn normalize_avatar(raw: &str) -> String {
    if raw.is_empty() {
        return String::new();
    }
    let with_scheme = match raw.strip_prefix("//") {
        Some(rest) => format!("https://{rest}"),
        None => raw.to_string(),
    };
    with_scheme.replace("_normal", "_bigger")
}

/// 推文正文。**长推文（note tweet）必须读 `note_tweet.note_text`**：
/// 这种情况下 `legacy.full_text` 是**被截断**的（以 `…` 结尾），只读它等于
/// 静默丢掉后半段——数据没报错，但内容是错的。
///
/// 拿到正文后再做两步清洗（与参考实现一致）：
/// 1. 去掉媒体自带的那条 t.co 链接（`entities.media[].url`）——它只是那张图的
///    占位文本，媒体本身已经在 `medias` 里了；
/// 2. 把其余 t.co 短链换成 `expanded_url`，让正文里是真实链接。
fn full_text_of(result: &Value, legacy: &Value) -> String {
    let note = result.get("note_tweet");
    let raw = note
        .and_then(|n| n.get("note_text"))
        .and_then(Value::as_str)
        .or_else(|| legacy.get("full_text").and_then(Value::as_str))
        .unwrap_or_default()
        .to_string();

    // 实体集有两处：长推文在自己的 `entity_set` 里（注意没有 `entities` 这层），
    // 普通推文在 `legacy.entities` 里。两处都过一遍——替换不到就是空操作。
    let mut text = raw;
    for entities in [
        note.and_then(|n| n.get("entity_set")),
        legacy.get("entities"),
    ]
    .into_iter()
    .flatten()
    {
        text = rewrite_tco_links(&text, entities);
    }
    text
}

fn rewrite_tco_links(text: &str, entities: &Value) -> String {
    let mut out = text.to_string();
    if let Some(media) = entities.get("media").and_then(Value::as_array) {
        for item in media {
            if let Some(short) = item.get("url").and_then(Value::as_str) {
                out = out.replace(short, "");
            }
        }
    }
    if let Some(urls) = entities.get("urls").and_then(Value::as_array) {
        for item in urls {
            let (Some(short), Some(expanded)) = (
                item.get("url").and_then(Value::as_str),
                item.get("expanded_url").and_then(Value::as_str),
            ) else {
                continue;
            };
            out = out.replace(short, expanded);
        }
    }
    out.trim().to_string()
}

/// 解析推文作者。**同时支持 legacy 与 core 两种结构**（见 `nested_str`）。
fn parse_author(result: &Value) -> Option<PostAuthor> {
    let user = result
        .get("core")
        .and_then(|c| c.get("user_results"))
        .and_then(|u| u.get("result"))?;
    let user = unwrap_visibility(user);

    // screen_name 是"这是个用户"的判据，两种结构里都有它
    let screen_name = nested_str(user, USER_SCREEN_NAME_PATHS)?;
    Some(PostAuthor {
        id: nested_str(user, USER_ID_PATHS)
            .unwrap_or_default()
            .to_string(),
        screen_name: screen_name.to_string(),
        name: nested_str(user, USER_NAME_PATHS)
            .unwrap_or_default()
            .to_string(),
        avatar: normalize_avatar(nested_str(user, USER_AVATAR_PATHS).unwrap_or_default()),
    })
}

/// 用户字段的候选路径：**老结构在前，新结构在后**（顺序无关正确性，只为可读）。
pub(crate) const USER_SCREEN_NAME_PATHS: &[&[&str]] =
    &[&["legacy", "screen_name"], &["core", "screen_name"]];
pub(crate) const USER_NAME_PATHS: &[&[&str]] = &[&["legacy", "name"], &["core", "name"]];
pub(crate) const USER_AVATAR_PATHS: &[&[&str]] = &[
    &["legacy", "profile_image_url_https"],
    &["avatar", "image_url"],
];
pub(crate) const USER_CREATED_AT_PATHS: &[&[&str]] =
    &[&["legacy", "created_at"], &["core", "created_at"]];
pub(crate) const USER_ID_PATHS: &[&[&str]] = &[&["rest_id"], &["id"]];

/// 取用户结构里的字段（供 `crate::user` 复用，保证两处解析规则一致）。
pub(crate) fn user_field(user: &Value, paths: &[&[&str]]) -> Option<String> {
    nested_str(user, paths).map(str::to_string)
}

fn parse_media(value: &Value) -> Option<Media> {
    let kind = MediaKind::from_x(value.get("type").and_then(|v| v.as_str())?)?;
    let id = str_field(value, "id_str")?;
    let variants = parse_variants(value);

    // 封面：X 对**每一种**媒体都给 `media_url_https`（视频/动图给的是静帧）
    let poster_url = str_field(value, "media_url_https");
    let url = match kind {
        MediaKind::Photo => poster_url.clone()?,
        // 视频/动图取**过滤掉 HLS 后码率最高**的变体（docs/02 §C7）。
        // 动图同样是 mp4，X 只在 video_info 里给 mp4 变体，所以规则一致。
        MediaKind::Video | MediaKind::AnimatedGif => best_variant_url(&variants)?,
    };

    let ext = ext_from_url(&url).unwrap_or_else(|| match kind {
        MediaKind::Photo => "jpg".to_string(),
        _ => "mp4".to_string(),
    });

    Some(Media {
        kind,
        id,
        url,
        ext,
        poster_url,
        width: value
            .get("original_info")
            .and_then(|o| o.get("width"))
            .and_then(|v| v.as_u64()),
        height: value
            .get("original_info")
            .and_then(|o| o.get("height"))
            .and_then(|v| v.as_u64()),
        duration_ms: value
            .get("video_info")
            .and_then(|v| v.get("duration_millis"))
            .and_then(|v| v.as_u64()),
        aspect_ratio: value
            .get("video_info")
            .and_then(|v| v.get("aspect_ratio"))
            .and_then(|v| v.as_array())
            .filter(|a| a.len() == 2)
            .and_then(|a| Some([a[0].as_u64()?, a[1].as_u64()?])),
        variants,
    })
}

/// 解析码率变体。**过滤掉 `application/x-mpegURL`**（HLS 播放列表不是可下载文件）。
///
/// 注意键名：上游 TS/Swift 读的是 `contentType`，但**实测响应里是 `content_type`**
/// （snake_case，152 处）。只认一种会让 `variants` 变成空数组 →
/// 视频/动图选不出可下载 URL → `parse_media` 返回 `None` →
/// 于是 `require_media` 把它筛掉，表现为"明明有媒体却整页解析成空"
/// （这是真实 fixture 抓出来的，见 AGENTS.md 踩坑记录）。所以两种都认。
fn parse_variants(value: &Value) -> Vec<MediaVariant> {
    value
        .get("video_info")
        .and_then(|v| v.get("variants"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| {
                    let content_type = v
                        .get("content_type")
                        .or_else(|| v.get("contentType"))
                        .and_then(|c| c.as_str())?;
                    if content_type == "application/x-mpegURL" {
                        return None;
                    }
                    let url = v.get("url").and_then(|u| u.as_str())?;
                    Some(MediaVariant {
                        url: url.to_string(),
                        content_type: content_type.to_string(),
                        bitrate: v.get("bitrate").and_then(|b| b.as_u64()),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 取码率最高的变体；没有码率的（动图）取第一条。
fn best_variant_url(variants: &[MediaVariant]) -> Option<String> {
    variants
        .iter()
        .max_by_key(|v| v.bitrate.unwrap_or(0))
        .map(|v| v.url.clone())
}

fn ext_from_url(url: &str) -> Option<String> {
    let path = url.split('?').next().unwrap_or(url);
    let last = path.rsplit('/').next()?;
    let ext = last.rsplit_once('.')?.1;
    if ext.is_empty() || ext.len() > 5 || !ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(ext.to_ascii_lowercase())
}

fn str_field(obj: &Value, key: &str) -> Option<String> {
    obj.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

/// 布尔字段。缺失按 `false`（X 的语义就是"没有这回事"）。
fn bool_field(obj: &Value, key: &str) -> bool {
    obj.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn u64_field(obj: &Value, key: &str) -> u64 {
    obj.get(key).and_then(|v| v.as_u64()).unwrap_or(0)
}

fn opt_u64_field(obj: &Value, key: &str) -> Option<u64> {
    obj.get(key).and_then(|v| v.as_u64())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tweet_result() -> Value {
        json!({
            "rest_id": "1234567890",
            "__typename": "Tweet",
            "views": { "count": "4321" },
            "legacy": {
                "created_at": "Wed Sep 30 12:34:56 +0000 2009",
                "full_text": "hello world",
                "lang": "en",
                "favorite_count": 10,
                "retweet_count": 3,
                "reply_count": 1,
                "bookmark_count": 7,
                "quote_count": 2,
                "possibly_sensitive": false,
                "favorited": true,
                "retweeted": true,
                "bookmarked": true,
                "in_reply_to_status_id_str": "999",
                "in_reply_to_screen_name": "someone_else",
                "entities": {
                    "hashtags": [{ "text": "rust" }, { "text": "x" }],
                    "media": [
                        {
                            "type": "photo",
                            "id_str": "111",
                            "media_url_https": "https://pbs.twimg.com/media/AAA.jpg",
                            "original_info": { "width": 1200, "height": 800 }
                        }
                    ]
                }
            },
            "core": {
                "user_results": {
                    "result": {
                        "rest_id": "42",
                        "legacy": {
                            "screen_name": "demo_user",
                            "name": "Demo User",
                            "profile_image_url_https": "https://pbs.twimg.com/profile_images/x.jpg"
                        }
                    }
                }
            }
        })
    }

    #[test]
    fn parses_a_normal_tweet() {
        let post = parse_post(&tweet_result()).expect("应能解析");
        assert_eq!(post.id, "1234567890");
        assert_eq!(post.full_text, "hello world");
        assert_eq!(post.created_at.as_deref(), Some("2009-09-30T12:34:56Z"));
        assert_eq!(post.views, Some(4321));
        assert_eq!(post.favorite_count, 10);
        assert_eq!(post.bookmark_count, Some(7));
        assert!(
            post.favorited && post.retweeted && post.bookmarked,
            "三面旗要读出来（UI 的实心/空心靠它）"
        );
        assert_eq!(post.tags, vec!["rust", "x"]);
        assert_eq!(post.author.screen_name, "demo_user");
        assert_eq!(post.author.id, "42");
        // 「回复 @xxx」必须是被回复者，不是本条作者
        assert_eq!(
            post.in_reply_to_screen_name.as_deref(),
            Some("someone_else")
        );
        assert_eq!(post.in_reply_to_id.as_deref(), Some("999"));
        assert_eq!(post.medias.len(), 1);
        assert_eq!(post.medias[0].kind, MediaKind::Photo);
        assert_eq!(post.medias[0].ext, "jpg");
        assert_eq!(post.medias[0].width, Some(1200));
    }

    #[test]
    fn tweet_unavailable_is_skipped_not_an_error() {
        let value =
            json!({ "rest_id": "1", "__typename": "TweetUnavailable", "reason": "Suspended" });
        assert!(parse_post(&value).is_none());
    }

    #[test]
    fn visibility_wrapper_is_unwrapped() {
        let wrapped = json!({
            "__typename": "TweetWithVisibilityResults",
            "tweet": tweet_result()
        });
        let post = parse_post(&wrapped).expect("包一层也要能解析");
        assert_eq!(post.id, "1234567890");
    }

    #[test]
    fn video_picks_the_highest_bitrate_and_drops_hls() {
        let value = json!({
            "rest_id": "1",
            "legacy": {
                "full_text": "v",
                "created_at": "Wed Sep 30 12:34:56 +0000 2009",
                "entities": { "media": [{
                    "type": "video",
                    "id_str": "m1",
                    "media_url_https": "https://pbs.twimg.com/ext_tw_video_thumb/1/pu/img/x.jpg",
                    "video_info": {
                        "duration_millis": 30000,
                        "aspect_ratio": [16, 9],
                        "variants": [
                            { "contentType": "application/x-mpegURL", "url": "https://video.twimg.com/x.m3u8" },
                            { "contentType": "video/mp4", "url": "https://video.twimg.com/low.mp4", "bitrate": 256000 },
                            { "contentType": "video/mp4", "url": "https://video.twimg.com/high.mp4", "bitrate": 2176000 }
                        ]
                    }
                }] }
            },
            "core": { "user_results": { "result": { "rest_id": "9", "legacy": { "screen_name": "a", "name": "A", "profile_image_url_https": "https://x/y.jpg" } } } }
        });
        let post = parse_post(&value).unwrap();
        let media = &post.medias[0];
        assert_eq!(media.kind, MediaKind::Video);
        assert_eq!(
            media.url, "https://video.twimg.com/high.mp4",
            "必须取最高码率"
        );
        assert_eq!(media.ext, "mp4");
        assert_eq!(media.duration_ms, Some(30000));
        assert_eq!(media.aspect_ratio, Some([16, 9]));
        assert_eq!(media.variants.len(), 2, "HLS 必须被过滤掉");
        assert!(media
            .variants
            .iter()
            .all(|v| v.content_type != "application/x-mpegURL"));
    }

    /// 实测：真实响应用 `content_type`（snake_case），而上游代码写的是 `contentType`。
    /// 两种都必须认——只认一种会让视频整条解析不出来（见 AGENTS.md 踩坑记录）。
    #[test]
    fn variants_are_read_with_both_key_spellings() {
        for key in ["content_type", "contentType"] {
            let value = json!({
                "rest_id": "1",
                "legacy": {
                    "full_text": "v",
                    "entities": { "media": [{
                        "type": "video",
                        "id_str": "m1",
                        "media_url_https": "https://pbs.twimg.com/t.jpg",
                        "video_info": { "variants": [
                            { key: "application/x-mpegURL", "url": "https://video.twimg.com/x.m3u8" },
                            { key: "video/mp4", "url": "https://video.twimg.com/real.mp4", "bitrate": 1000 }
                        ] }
                    }] }
                },
                "core": { "user_results": { "result": { "rest_id": "9", "legacy": {
                    "screen_name": "a", "name": "A", "profile_image_url_https": "https://x/y.jpg" } } } }
            });
            let post = parse_post(&value).unwrap_or_else(|| panic!("{key} 拼写下必须能解析"));
            assert_eq!(post.medias.len(), 1, "{key}");
            assert_eq!(
                post.medias[0].url, "https://video.twimg.com/real.mp4",
                "{key}"
            );
            assert_eq!(post.medias[0].variants.len(), 1, "{key}：HLS 要被过滤掉");
        }
    }

    #[test]
    fn animated_gif_uses_the_mp4_variant() {
        let value = json!({
            "rest_id": "1",
            "legacy": {
                "full_text": "g",
                "entities": { "media": [{
                    "type": "animated_gif",
                    "id_str": "m2",
                    "media_url_https": "https://pbs.twimg.com/t.jpg",
                    "video_info": { "variants": [
                        { "contentType": "video/mp4", "url": "https://video.twimg.com/tweet_video/g.mp4" }
                    ] }
                }] }
            },
            "core": { "user_results": { "result": { "rest_id": "9", "legacy": { "screen_name": "a", "name": "A", "profile_image_url_https": "https://x/y.jpg" } } } }
        });
        let post = parse_post(&value).unwrap();
        assert_eq!(post.medias[0].kind, MediaKind::AnimatedGif);
        assert_eq!(
            post.medias[0].url,
            "https://video.twimg.com/tweet_video/g.mp4"
        );
    }

    #[test]
    fn photo_thumbnail_and_original_add_name_param() {
        let post = parse_post(&tweet_result()).unwrap();
        let m = &post.medias[0];
        assert_eq!(
            m.thumbnail_url(),
            "https://pbs.twimg.com/media/AAA.jpg?name=small"
        );
        assert_eq!(
            m.original_url(),
            "https://pbs.twimg.com/media/AAA.jpg?name=orig"
        );
        // 原 url 本身不带 query，避免外壳再拼一次时出现两个 ?
        assert!(!m.url.contains('?'));
    }

    #[test]
    fn missing_created_at_keeps_the_post() {
        // docs/02 §D4：无 createdAt 的条目要放行，不能因为解析不到时间就丢掉
        let mut value = tweet_result();
        value["legacy"]
            .as_object_mut()
            .unwrap()
            .remove("created_at");
        let post = parse_post(&value).expect("没有 created_at 也必须保留");
        assert_eq!(post.created_at, None);
    }

    #[test]
    fn counts_are_defaulted_to_zero_but_bookmark_stays_optional() {
        let mut value = tweet_result();
        let legacy = value["legacy"].as_object_mut().unwrap();
        legacy.remove("favorite_count");
        legacy.remove("bookmark_count");
        let post = parse_post(&value).unwrap();
        assert_eq!(
            post.favorite_count, 0,
            "计数缺失按 0（X 的行为就是没有计数）"
        );
        assert_eq!(
            post.bookmark_count, None,
            "可选计数缺失保持 None，不伪装成 0"
        );
    }

    #[test]
    fn retweet_is_unwrapped_and_the_retweeter_recorded() {
        let mut wrapper = tweet_result();
        wrapper["rest_id"] = json!("outer");
        wrapper["legacy"]["retweeted_status_result"] = json!({
            "result": {
                "rest_id": "inner",
                "legacy": { "full_text": "original", "created_at": "Wed Sep 30 12:34:56 +0000 2009" },
                "core": { "user_results": { "result": { "rest_id": "1", "legacy": { "screen_name": "orig", "name": "O", "profile_image_url_https": "https://x/o.jpg" } } } }
            }
        });
        let (post, retweeter) = unwrap_retweet(&wrapper).expect("应能拆出原推");
        assert_eq!(post.id, "inner");
        assert_eq!(post.full_text, "original");
        assert_eq!(retweeter.unwrap().screen_name, "demo_user");
    }

    #[test]
    fn ext_comes_from_url_and_falls_back_by_kind() {
        assert_eq!(ext_from_url("https://a/b/c.JPG"), Some("jpg".into()));
        assert_eq!(
            ext_from_url("https://a/b/c.jpg?name=small"),
            Some("jpg".into())
        );
        assert_eq!(ext_from_url("https://a/b/c"), None);
        assert_eq!(ext_from_url("https://a/b/c.notanextension"), None);
    }

    /// 实测：同一端点在不同响应里给了两种用户结构（search/home/following 用 core+avatar）。
    /// 只认 legacy 会让那半边响应整页解析失败。
    #[test]
    fn new_style_author_is_parsed() {
        let mut value = tweet_result();
        value["core"]["user_results"]["result"] = json!({
            "__typename": "User",
            "rest_id": "42",
            "id": "VXNlcjo0Mg==",
            "core": { "screen_name": "demo_user", "name": "Demo User" },
            "avatar": { "image_url": "https://pbs.twimg.com/profile_images/1/x_normal.jpg" }
        });
        let post = parse_post(&value).expect("新结构必须能解析");
        assert_eq!(post.author.screen_name, "demo_user");
        assert_eq!(post.author.name, "Demo User");
        assert_eq!(post.author.id, "42");
        assert!(
            post.author.avatar.starts_with("https://pbs.twimg.com/"),
            "头像要取自 avatar.image_url，实际 {}",
            post.author.avatar
        );
    }

    #[test]
    fn user_id_falls_back_to_id_when_rest_id_is_absent() {
        let mut value = tweet_result();
        value["core"]["user_results"]["result"] = json!({
            "__typename": "User",
            "id": "VXNlcjo0Mg==",
            "core": { "screen_name": "demo_user", "name": "Demo User" },
            "avatar": { "image_url": "https://pbs.twimg.com/a.jpg" }
        });
        let post = parse_post(&value).unwrap();
        assert_eq!(post.author.id, "VXNlcjo0Mg==");
    }

    #[test]
    fn post_without_an_author_is_skipped() {
        let mut value = tweet_result();
        value.as_object_mut().unwrap().remove("core");
        assert!(parse_post(&value).is_none());
    }

    #[test]
    fn post_without_rest_id_is_skipped() {
        let mut value = tweet_result();
        value.as_object_mut().unwrap().remove("rest_id");
        assert!(parse_post(&value).is_none());
    }

    /// 长推文（note tweet）：`legacy.full_text` 是**被截断**的，
    /// 正文在 `note_tweet.note_text` 里。只读 legacy 会静默丢掉后半段。
    #[test]
    fn long_tweet_prefers_note_text_and_cleans_tco_links() {
        let mut value = tweet_result();
        value["legacy"]["full_text"] = json!("正文前半 https://t.co/link 还有更…");
        value["note_tweet"] = json!({
            "note_text": "正文前半 https://t.co/link 还有更多内容 https://t.co/mediaTail",
            "entity_set": {
                "urls": [{ "url": "https://t.co/link", "expanded_url": "https://example.com/a/long/path" }],
                "media": [{ "url": "https://t.co/mediaTail" }]
            }
        });
        let post = parse_post(&value).unwrap();
        assert_eq!(
            post.full_text, "正文前半 https://example.com/a/long/path 还有更多内容",
            "要用 note_text，且短链展开、媒体链接去掉"
        );
    }

    /// 普通推文：媒体自己的 t.co 链接只是那张图的占位文本，正文里应当去掉；
    /// 其余短链换成 `expanded_url`。与参考实现（`x-spider-mac` 的 `mapTwitterPost`）一致。
    #[test]
    fn plain_text_loses_the_media_link_and_expands_urls() {
        let mut value = tweet_result();
        value["legacy"]["full_text"] = json!("看图 https://t.co/pic 参考 https://t.co/doc");
        value["legacy"]["entities"]["media"][0]["url"] = json!("https://t.co/pic");
        value["legacy"]["entities"]["urls"] = json!([
            { "url": "https://t.co/doc", "expanded_url": "https://docs.example.com/page" }
        ]);
        let post = parse_post(&value).unwrap();
        assert_eq!(
            post.full_text,
            "看图  参考 https://docs.example.com/page".trim()
        );
    }

    /// 头像归一化：`//` → `https://`，`_normal` → `_bigger`
    /// ——与参考实现给外壳的值一致，接入时外壳那两行替换就可以删掉。
    #[test]
    fn avatar_is_normalized_to_https_and_bigger_size() {
        assert_eq!(
            normalize_avatar("//pbs.twimg.com/profile_images/1/x_normal.jpg"),
            "https://pbs.twimg.com/profile_images/1/x_bigger.jpg"
        );
        assert_eq!(
            normalize_avatar("https://pbs.twimg.com/profile_images/1/x.png"),
            "https://pbs.twimg.com/profile_images/1/x.png"
        );
        assert_eq!(normalize_avatar(""), "");
    }
}
