//! **契约守卫**：文档、schema 与代码三者不允许各说各话。
//!
//! 这个文件的存在本身就是一条规则（`docs/05-WORKFLOW.md` §7）：
//! 「不一致的文档比没有文档更坏」。所以不一致必须是**测试失败**，
//! 而不是某天有人读文档时才发现。
//!
//! 覆盖三件事：
//! 1. schema 的 method 枚举 ⟷ 代码里登记的 method 集合**相等**；
//! 2. schema 的 per-method 明细覆盖每个 method，且 `CONTRACT.md` 提到每一个；
//! 3. `error` 对象与 `user` 对象的**字段集合**与 schema 一致
//!    （新增一个字段却忘了改 schema，会在这里红）。

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::Value;
use xspider_core::error::{ErrorCode, XError};
use xspider_core::paging::Page;
use xspider_download::{DownloadEvent, Integrity};
use xspider_fetch::{Media, MediaKind, MediaVariant, Post, PostAuthor, User};

fn workspace_file(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

fn read_json(rel: &str) -> Value {
    let path = workspace_file(rel);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("读取 {} 失败：{e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{} 不是合法 JSON：{e}", path.display()))
}

fn read_text(rel: &str) -> String {
    let path = workspace_file(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("读取 {} 失败：{e}", path.display()))
}

fn strings(value: &Value) -> BTreeSet<String> {
    value
        .as_array()
        .expect("期望数组")
        .iter()
        .map(|v| v.as_str().expect("期望字符串").to_string())
        .collect()
}

fn implemented() -> BTreeSet<String> {
    xspider::METHODS.iter().map(|m| (*m).to_string()).collect()
}

/// **sidecar 传输层**独有的 method（`docs/CONTRACT.md` §3.2）。
///
/// 它们不属于契约载荷：cdylib 形态没有"关掉宿主进程"这回事。
/// 白名单写在这里而不是"跳过所有表格行"，是为了让新增例外时必须改代码——
/// 例外一旦可以被静默添加，这个守卫就失效了。
const SIDECAR_ONLY_METHODS: &[&str] = &["system.shutdown"];

#[test]
fn schema_method_enum_equals_implemented_methods() {
    let schema = read_json("contract/xspider.schema.json");
    let in_schema = strings(&schema["$defs"]["method"]["enum"]);
    assert_eq!(
        in_schema,
        implemented(),
        "schema 的 method 枚举与 xspider::METHODS 不一致——\\
         契约与实现必须同时改（先改文档，再改代码）"
    );
}

#[test]
fn schema_details_cover_every_method() {
    let schema = read_json("contract/xspider.schema.json");
    let detailed = schema["x-methods"]
        .as_object()
        .expect("schema 缺少 x-methods")
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    assert_eq!(
        detailed,
        implemented(),
        "x-methods 必须逐个 method 给出 params/result 形状"
    );
}

#[test]
fn contract_markdown_documents_every_method() {
    let text = read_text("docs/CONTRACT.md");
    for method in implemented() {
        assert!(
            text.contains(&method),
            "docs/CONTRACT.md 里没有提到已实现的 method {method:?}"
        );
    }

    // 反向粗筛：表格首列里出现的 `system.*` 名字必须是真的登记过的
    // （或是显式白名单里的 sidecar 传输层 method）。
    let hint = "若它是 sidecar 传输层 method，请加进 SIDECAR_ONLY_METHODS 并同步文档";
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("| `") else {
            continue;
        };
        let Some((name, _)) = rest.split_once('`') else {
            continue;
        };
        let looks_like_method = name.contains('.')
            && !name.contains(' ')
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_');
        if looks_like_method && name.starts_with("system.") {
            assert!(
                implemented().contains(name) || SIDECAR_ONLY_METHODS.contains(&name),
                "CONTRACT.md 表格里出现了未登记的 method {name:?}（{hint}）"
            );
        }
    }
}

#[test]
fn error_codes_in_schema_match_the_code_enum() {
    let schema = read_json("contract/xspider.schema.json");
    let in_schema = strings(&schema["$defs"]["error"]["properties"]["code"]["enum"]);

    // 每个错误码都造一个真实的错误，序列化后取 code
    let samples: Vec<XError> = vec![
        XError::invalid_request("x"),
        XError::unauthorized("x"),
        XError::RateLimited {
            retry_after_s: 1,
            endpoint: None,
        },
        XError::not_found("x"),
        XError::upstream(500, "x"),
        XError::parse("c", "x"),
        XError::transport("timeout", "x"),
        XError::Cancelled,
        XError::internal("x"),
    ];
    let produced: BTreeSet<String> = samples
        .iter()
        .map(|e| {
            serde_json::to_value(e.to_object()).unwrap()["code"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(
        produced, in_schema,
        "Rust 侧的 ErrorCode 与 schema 的 code 枚举不一致"
    );
    // 顺带确认枚举本身是穷尽的（新增变体时这里会红）
    assert_eq!(
        in_schema.len(),
        9,
        "错误码数量变了：记得同步 schema 与 CONTRACT.md"
    );
    let _ = ErrorCode::Internal;
}

#[test]
fn every_error_field_is_declared_in_the_schema() {
    let schema = read_json("contract/xspider.schema.json");
    let declared: BTreeSet<String> = schema["$defs"]["error"]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();

    // 造一批"字段最全"的错误（每种变体各一个），把出现过的键都收集起来
    let samples = [
        XError::invalid_request("x"),
        XError::unauthorized("x").with_endpoint("e"),
        XError::RateLimited {
            retry_after_s: 5,
            endpoint: Some("e".into()),
        },
        XError::not_found("x"),
        XError::upstream(503, "x"),
        XError::parse("ctx", "x"),
        XError::transport("timeout", "x"),
        XError::Cancelled,
        XError::internal("x"),
    ];
    let mut produced: BTreeSet<String> = BTreeSet::new();
    for e in samples {
        let obj = serde_json::to_value(e.to_object()).unwrap();
        for key in obj.as_object().unwrap().keys() {
            produced.insert(key.clone());
        }
    }
    let undeclared: Vec<&String> = produced.difference(&declared).collect();
    assert!(
        undeclared.is_empty(),
        "错误对象里有 schema 未声明的字段：{undeclared:?}（schema 用 additionalProperties:false，会导致校验失败）"
    );
}

/// 一个"字段全填满"的 Post，用来核对 schema 覆盖面。
fn sample_post() -> Post {
    Post {
        id: "1".into(),
        created_at: Some("2009-09-30T12:34:56Z".into()),
        full_text: "hello".into(),
        lang: Some("en".into()),
        views: Some(1),
        favorite_count: 0,
        retweet_count: 0,
        reply_count: 0,
        bookmark_count: Some(0),
        quote_count: Some(0),
        possibly_sensitive: false,
        favorited: false,
        retweeted: false,
        bookmarked: false,
        medias: vec![Media {
            kind: MediaKind::Video,
            id: "m1".into(),
            url: "https://video.twimg.com/redacted/demo.mp4".into(),
            ext: "mp4".into(),
            poster_url: Some("https://pbs.twimg.com/redacted/demo.jpg".into()),
            width: Some(1920),
            height: Some(1080),
            duration_ms: Some(1000),
            aspect_ratio: Some([16, 9]),
            variants: vec![MediaVariant {
                url: "https://video.twimg.com/redacted/demo.mp4".into(),
                content_type: "video/mp4".into(),
                bitrate: Some(1000),
            }],
        }],
        author: PostAuthor {
            id: "2".into(),
            screen_name: "demo_user".into(),
            name: "Demo User".into(),
            avatar: "https://pbs.twimg.com/redacted/demo.jpg".into(),
        },
        tags: vec!["rust".into()],
        quoted_id: Some("3".into()),
        in_reply_to_screen_name: Some("other".into()),
        in_reply_to_id: Some("4".into()),
        retweeted_by: Some(PostAuthor {
            id: "5".into(),
            screen_name: "demo_user".into(),
            name: "Demo User".into(),
            avatar: "https://pbs.twimg.com/redacted/demo.jpg".into(),
        }),
    }
}

fn schema_properties(schema: &Value, def: &str) -> BTreeSet<String> {
    schema["$defs"][def]["properties"]
        .as_object()
        .unwrap_or_else(|| panic!("schema 缺少 $defs.{def}.properties"))
        .keys()
        .cloned()
        .collect()
}

#[test]
fn post_dto_fields_match_the_schema() {
    let schema = read_json("contract/xspider.schema.json");
    let declared = schema_properties(&schema, "post");
    let required = strings(&schema["$defs"]["post"]["required"]);

    let rendered = serde_json::to_value(sample_post()).unwrap();
    let keys: BTreeSet<String> = rendered.as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        keys, declared,
        "Post DTO 的字段与 schema 的 post 定义不一致"
    );
    for field in required {
        assert!(
            keys.contains(&field),
            "schema 声明 {field} 必填，但 Post DTO 里没有"
        );
    }

    // 嵌套的 media / author 也要对得上
    assert_eq!(
        rendered["medias"][0]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        schema_properties(&schema, "media"),
        "Media DTO 的字段与 schema 的 media 定义不一致"
    );
    assert_eq!(
        rendered["author"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        schema_properties(&schema, "postAuthor"),
        "PostAuthor DTO 的字段与 schema 的 postAuthor 定义不一致"
    );
    assert_eq!(
        rendered["medias"][0]["variants"][0]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>(),
        schema_properties(&schema, "mediaVariant"),
        "MediaVariant DTO 的字段与 schema 的 mediaVariant 定义不一致"
    );
}

/// `reply` 是"post 的字段 + 两个回复特有字段"，且共享字段的定义必须**逐字相同**。
/// 这条守的是"有人给 Post 加了字段却忘了给 Reply 加"（或反过来）。
#[test]
fn reply_schema_is_post_plus_two_fields() {
    let schema = read_json("contract/xspider.schema.json");
    let post = schema_properties(&schema, "post");
    let reply = schema_properties(&schema, "reply");

    let extra: BTreeSet<String> = reply.difference(&post).cloned().collect();
    assert_eq!(
        extra,
        ["parent_id".to_string(), "is_partial_parent".to_string()]
            .into_iter()
            .collect::<BTreeSet<_>>(),
        "reply 只应比 post 多 parent_id 与 is_partial_parent"
    );
    assert!(
        post.difference(&reply).next().is_none(),
        "reply 缺少 post 的某些字段：{:?}",
        post.difference(&reply).collect::<Vec<_>>()
    );
    for field in post {
        assert_eq!(
            schema["$defs"]["reply"]["properties"][&field],
            schema["$defs"]["post"]["properties"][&field],
            "字段 {field} 在 post 与 reply 里的定义必须一致（否则外壳解析回复会踩坑）"
        );
    }
}

/// 分页形状：`items` / 可选 `cursor` / `end`。
/// **所有**分页端点必须同形，否则外壳得为每个端点写一套翻页代码。
#[test]
fn page_shape_is_uniform_and_matches_the_schema() {
    let schema = read_json("contract/xspider.schema.json");
    for def in ["postPage", "userPage"] {
        assert_eq!(
            schema_properties(&schema, def),
            ["items", "cursor", "end"]
                .into_iter()
                .map(str::to_string)
                .collect::<BTreeSet<_>>(),
            "{def} 的字段集合不对"
        );
        let required = strings(&schema["$defs"][def]["required"]);
        assert_eq!(
            required,
            ["items", "end"].into_iter().map(str::to_string).collect(),
            "{def} 的必填集合不对：cursor 缺省时该键不出现（= 到底）"
        );
    }

    // 空页序列化后不能带 cursor 键
    let empty: Page<User> = Page::empty();
    let rendered = serde_json::to_value(&empty).unwrap();
    assert_eq!(rendered["end"], true);
    assert!(
        rendered.get("cursor").is_none(),
        "空页不该出现 cursor 键：{rendered}"
    );
}

/// 契约文档必须提到每个已实现的 method（反向由 `contract_markdown_documents_every_method` 守）。
/// `dl.events` 的事件形状必须写进 schema——这是**消费者逼出来的**一条：
/// CLI 只能照着 `CONTRACT.md` 的散文猜 `kind` / `integrity` 长什么样，
/// 而机器可读的那份契约当时写的是 `{"items": {"type": "object"}}`（等于没说）。
#[test]
fn download_event_shapes_match_the_schema() {
    let schema = read_json("contract/xspider.schema.json");
    let branches = schema["$defs"]["downloadEvent"]["oneOf"]
        .as_array()
        .expect("downloadEvent 应当是 oneOf");
    // kind → 该分支声明的字段集合
    let mut declared: BTreeSet<(String, String)> = BTreeSet::new();
    for branch in branches {
        let kind = branch["properties"]["kind"]["const"]
            .as_str()
            .expect("每个分支都要有 kind 常量")
            .to_string();
        for field in strings(&branch["required"]) {
            declared.insert((kind.clone(), field));
        }
        // additionalProperties:false 是刻意的：拼错字段名不该被静默忽略
        assert_eq!(branch["additionalProperties"], false, "{kind} 分支");
    }

    let events = [
        DownloadEvent::Progress {
            job_id: "j1".into(),
            done: 10,
            total: 100,
        },
        DownloadEvent::Completed {
            job_id: "j1".into(),
            path: "/tmp/a.mp4".into(),
            bytes: 100,
            integrity: Integrity::Verified {
                expected: 100,
                actual: 100,
            },
        },
        DownloadEvent::Failed {
            job_id: "j1".into(),
            reason: "not_found".into(),
            error: Box::new(XError::not_found("没了").to_object()),
        },
        DownloadEvent::Skipped {
            job_id: "j1".into(),
            reason: "already_present".into(),
        },
    ];

    for event in events {
        let value = serde_json::to_value(&event).expect("事件应当能序列化");
        let kind = value["kind"].as_str().expect("事件必须带 kind").to_string();
        let actual: BTreeSet<(String, String)> = value
            .as_object()
            .expect("事件是对象")
            .keys()
            .map(|k| (kind.clone(), k.clone()))
            .collect();
        let expected: BTreeSet<(String, String)> = declared
            .iter()
            .filter(|(k, _)| k == &kind)
            .cloned()
            .collect();
        assert!(!expected.is_empty(), "schema 里没有 {kind} 这种事件");
        assert_eq!(
            actual, expected,
            "{kind} 事件的字段与 schema 不一致（新增字段要同步改 schema）"
        );
    }
}

#[test]
fn fetch_methods_are_documented_with_their_paging_convention() {
    let text = read_text("docs/CONTRACT.md");
    for method in [
        "fetch.user_medias",
        "fetch.user_tweets",
        "fetch.tweet_detail",
        "fetch.search_timeline",
        "fetch.home_timeline",
        "fetch.following",
    ] {
        assert!(text.contains(method), "CONTRACT.md 缺少 {method} 的说明");
    }
    // 分页约定必须写清楚（这是最容易踩的一条）
    assert!(
        text.contains("单页 + 游标，不自动翻页"),
        "CONTRACT.md 必须写明分页约定"
    );
    assert!(
        text.contains("外壳不要再自己加一天"),
        "CONTRACT.md 必须写明 until 的 +1 天由组件负责，外壳不要再加"
    );
}
#[test]
fn user_dto_fields_match_the_schema() {
    let schema = read_json("contract/xspider.schema.json");
    let declared: BTreeSet<String> = schema["$defs"]["user"]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    let required = strings(&schema["$defs"]["user"]["required"]);

    let sample = User {
        id: "1".into(),
        screen_name: "demo_user".into(),
        name: "Demo User".into(),
        avatar: "https://pbs.twimg.com/redacted/demo.jpg".into(),
        media_count: Some(1),
        register_time: Some("2009-09-30T12:34:56Z".into()),
    };
    let rendered = serde_json::to_value(&sample).unwrap();
    let keys: BTreeSet<String> = rendered.as_object().unwrap().keys().cloned().collect();

    assert_eq!(
        keys, declared,
        "User DTO 的字段与 schema 的 user 定义不一致"
    );
    for field in required {
        assert!(
            keys.contains(&field),
            "schema 声明 {field} 必填，但 DTO 里没有"
        );
    }
    // 缺失的 Optional 字段必须以 null 出现（而不是干脆没有这个键）：
    // 外壳按固定 schema 解析时，"键不存在"和"值为 null"是两种代码路径
    let minimal = User {
        id: "1".into(),
        screen_name: "s".into(),
        name: "n".into(),
        avatar: "a".into(),
        media_count: None,
        register_time: None,
    };
    let rendered = serde_json::to_value(&minimal).unwrap();
    assert!(rendered.get("media_count").is_some_and(Value::is_null));
    assert!(rendered.get("register_time").is_some_and(Value::is_null));
}

#[test]
fn request_and_envelope_shapes_are_declared() {
    let schema = read_json("contract/xspider.schema.json");
    // 请求体：id / method / params / token
    let req = schema["$defs"]["request"]["properties"]
        .as_object()
        .unwrap();
    for key in ["id", "method", "params", "token"] {
        assert!(req.contains_key(key), "请求体 schema 缺少 {key}");
    }
    // 两种包络
    assert!(schema["$defs"]["resultEnvelope"]["required"]
        .as_array()
        .unwrap()
        .contains(&Value::String("result".into())));
    assert!(schema["$defs"]["errorEnvelope"]["required"]
        .as_array()
        .unwrap()
        .contains(&Value::String("error".into())));
}
