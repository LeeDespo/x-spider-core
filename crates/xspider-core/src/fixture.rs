//! fixture：本仓库最重要的测试资产（`docs/04-TESTING-AND-FIXTURES.md` §2）。
//!
//! 铁律回顾：**fixture 必须是真实抓到的响应**，不许手写 JSON 冒充。
//! 手写的样本只能证明「你按自己以为的格式解析正确」，X 改字段时它不会红——
//! 那正是「用户先发现问题」的根因。
//!
//! 这里的结构只做两件事：**原样保存**上游响应（status/headers/body），
//! 以及**按请求路由**到对应场景。脱敏在录制流程里做（`script/redact_fixtures.py`），
//! 不在回放里做——回放看到什么，测试就断言什么。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::{XError, XResult};
use crate::transport::{HttpMethod, HttpRequest, HttpResponse, TransportError, TransportResult};

/// 请求该带什么凭据才能命中这条 fixture。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMatch {
    /// 请求必须带 cookie。
    Valid,
    /// 请求必须不带 cookie（用于抓「未登录 → 401」这种真实响应）。
    None,
    /// 不关心。
    Any,
}

/// 请求有没有带游标。
///
/// 为什么需要它：分页端点里"第一页"和"第二页"的**其它特征完全一样**
/// （同一个 user_id、同一个 operation），只能靠游标的有无来区分。
/// 没有这一项就没法同时录"首页"和"翻页"两条 fixture——而"翻页要带上一页的游标"
/// 正是 `docs/04` §3 要求钉住的语义。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorMatch {
    /// 请求里**不能**有 cursor（首页）。
    Absent,
    /// 请求里**必须**有 cursor（翻页）。
    Present,
}

/// fixture 的路由条件。**越具体越优先**（见 `pick`）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FixtureMatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// URL path 的最后一段，例如 `UserByScreenName`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    /// 从 query 的 `variables` 里取出的 `screen_name`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen_name: Option<String>,
    /// 游标的有无。分页端点靠它区分首页与翻页。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<CursorMatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthMatch>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixtureResponse {
    pub status: u16,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// 原样保存的响应体。录制时能解析成 JSON 就存 JSON，否则存字符串。
    pub body: serde_json::Value,
}

/// 一条 fixture = 一次真实请求的请求特征 + 上游的原样响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fixture {
    /// 逻辑端点名（文档用，不参与路由），例如 `user_by_screen_name`。
    pub endpoint: String,
    /// 场景名：`normal` / `empty_end` / `rate_limited` / `not_found` / `field_changed` …
    pub scenario: String,
    /// 采集这条 fixture 的日期（`YYYY-MM-DD`），改版样本靠它排时间线。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captured_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(rename = "match", default)]
    pub match_: FixtureMatch,
    pub response: FixtureResponse,
}

#[derive(Debug, Clone)]
struct Loaded {
    path: PathBuf,
    fixture: Fixture,
}

/// 从目录树加载的 fixture 集合。
#[derive(Debug, Clone)]
pub struct ReplayTransport {
    fixtures: Arc<Vec<Loaded>>,
    root: PathBuf,
}

impl ReplayTransport {
    /// 递归加载 `<dir>` 下所有 `*.json`（跳过录制原始目录 `raw/`）。
    pub fn load(dir: impl AsRef<Path>) -> XResult<Self> {
        let root = dir.as_ref().to_path_buf();
        let mut files = Vec::new();
        collect_json(&root, &mut files)?;
        files.sort();
        let mut fixtures = Vec::with_capacity(files.len());
        for path in files {
            let text = std::fs::read_to_string(&path).map_err(|e| {
                XError::internal(format!("读取 fixture {} 失败：{e}", path.display()))
            })?;
            let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
                XError::internal(format!("fixture {} 不是合法 JSON：{e}", path.display()))
            })?;
            // `fixtures/` 下除了 HTTP 响应样本，还住着别的测试素材
            // （例如 `xclid/page_artifacts.json` 是页面抠出来的签名原料）。
            // 判据用**结构**而不是文件名：必须有 `response`。
            //
            // 静默跳过是安全的，因为"fixture 少了"从来不会安静地通过：
            // 回放时匹配不到会直接报 fixture 错误（见 `execute`）。
            if value.get("response").is_none() {
                tracing::debug!(
                    path = %path.display(),
                    "跳过：这不是 HTTP 响应 fixture（没有 response 字段）"
                );
                continue;
            }
            let fixture: Fixture = serde_json::from_value(value).map_err(|e| {
                XError::internal(format!(
                    "fixture {} 结构不对（有 response 但其它字段缺失）：{e}",
                    path.display()
                ))
            })?;
            fixtures.push(Loaded { path, fixture });
        }
        Ok(Self {
            fixtures: Arc::new(fixtures),
            root,
        })
    }

    pub fn len(&self) -> usize {
        self.fixtures.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fixtures.is_empty()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 已加载的场景列表（`<scenario> @ <relative path>`），冒烟脚本与测试用来核对覆盖度。
    pub fn scenarios(&self) -> Vec<String> {
        self.fixtures
            .iter()
            .map(|l| {
                let rel = l
                    .path
                    .strip_prefix(&self.root)
                    .unwrap_or(&l.path)
                    .display()
                    .to_string();
                format!("{} @ {}", l.fixture.scenario, rel)
            })
            .collect()
    }

    pub async fn execute(&self, req: &HttpRequest) -> TransportResult {
        let picked = self.pick(req).ok_or_else(|| {
            TransportError::new(
                "fixture",
                format!(
                    "没有匹配的 fixture：{} {} screen_name={:?} auth={}（目录 {}）",
                    req.method.as_str(),
                    operation_of(&req.url).unwrap_or_else(|| "?".into()),
                    screen_name_of(&req.url),
                    if req.has_cookie() { "valid" } else { "none" },
                    self.root.display()
                ),
            )
        })?;
        Ok(response_of(&picked.fixture))
    }

    fn pick(&self, req: &HttpRequest) -> Option<&Loaded> {
        let operation = operation_of(&req.url);
        let screen_name = screen_name_of(&req.url);
        let has_cookie = req.has_cookie();
        let has_cursor = cursor_state_of(req);

        self.fixtures
            .iter()
            .filter_map(|l| {
                score(
                    &l.fixture.match_,
                    req.method,
                    operation.as_deref(),
                    screen_name.as_deref(),
                    has_cookie,
                    has_cursor,
                )
                .map(|s| (s, l))
            })
            .max_by(|(sa, a), (sb, b)| sa.cmp(sb).then_with(|| b.path.cmp(&a.path)))
            .map(|(_, l)| l)
    }
}

/// 匹配打分：不匹配返回 `None`；匹配则返回具体度，越大越优先。
fn score(
    m: &FixtureMatch,
    method: HttpMethod,
    operation: Option<&str>,
    screen_name: Option<&str>,
    has_cookie: bool,
    has_cursor: Option<bool>,
) -> Option<u32> {
    let mut score = 0u32;
    if let Some(want) = &m.method {
        if !want.eq_ignore_ascii_case(method.as_str()) {
            return None;
        }
        score += 1;
    }
    if let Some(want) = &m.operation {
        if Some(want.as_str()) != operation {
            return None;
        }
        score += 2;
    }
    if let Some(want) = &m.screen_name {
        if Some(want.as_str()) != screen_name {
            return None;
        }
        score += 4;
    }
    if let Some(want) = m.cursor {
        // 问不出 cursor（没带 variables，例如探测页）→ 带此条件的 fixture 不参与匹配
        let actual = has_cursor?;
        let matches = match want {
            CursorMatch::Absent => !actual,
            CursorMatch::Present => actual,
        };
        if !matches {
            return None;
        }
        score += 8;
    }
    match m.auth.unwrap_or(AuthMatch::Any) {
        AuthMatch::Valid => {
            if !has_cookie {
                return None;
            }
            score += 2;
        }
        AuthMatch::None => {
            if has_cookie {
                return None;
            }
            score += 2;
        }
        AuthMatch::Any => {}
    }
    Some(score)
}

fn response_of(f: &Fixture) -> HttpResponse {
    let body = match &f.response.body {
        serde_json::Value::String(s) => s.clone().into_bytes(),
        other => other.to_string().into_bytes(),
    };
    let mut headers: Vec<(String, String)> = f
        .response
        .headers
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if !headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
    {
        headers.push(("content-type".into(), "application/json".into()));
    }
    HttpResponse {
        status: f.response.status,
        headers,
        body,
    }
}

fn collect_json(dir: &Path, out: &mut Vec<PathBuf>) -> XResult<()> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| XError::internal(format!("读取目录 {} 失败：{e}", dir.display())))?;
    for entry in entries {
        let entry =
            entry.map_err(|e| XError::internal(format!("遍历 {} 失败：{e}", dir.display())))?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            // `raw/` 是录制原始响应，脱敏后才入库
            if name == "raw" || name.starts_with('.') {
                continue;
            }
            collect_json(&path, out)?;
        } else if name.ends_with(".json") {
            out.push(path);
        }
    }
    Ok(())
}

/// URL path 的最后一段（= 上游的 operationName）。
pub fn operation_of(url: &str) -> Option<String> {
    let without_query = url.split(['?', '#']).next().unwrap_or(url);
    let last = without_query.rsplit('/').find(|s| !s.is_empty())?;
    Some(last.to_string())
}

/// 请求里有没有带 `cursor`。
///
/// - 返回 `None` 表示**问不出这个问题**（请求里没有 `variables`，例如探测页）；
///   此时带 `cursor` 条件的 fixture 不参与匹配，避免误命中；
/// - GET 从 URL query 的 `variables` 看，POST 从 body 的 `variables` 看
///   （两个端点族的参数位置不同，`docs/02` §A2）。
pub fn cursor_state_of(req: &HttpRequest) -> Option<bool> {
    let variables = variables_of(req)?;
    Some(variables.get("cursor").is_some())
}

fn variables_of(req: &HttpRequest) -> Option<serde_json::Value> {
    if let Some(body) = &req.body {
        let parsed: serde_json::Value = serde_json::from_slice(body).ok()?;
        return parsed.get("variables").cloned();
    }
    let query = req.url.split_once('?')?.1;
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=')?;
        if k == "variables" {
            return serde_json::from_str(&percent_decode(v)).ok();
        }
    }
    None
}

/// 从 query 的 `variables` 里取 `screen_name`（fixture 路由用）。
pub fn screen_name_of(url: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=')?;
        if k != "variables" {
            continue;
        }
        let decoded = percent_decode(v);
        let json: serde_json::Value = serde_json::from_str(&decoded).ok()?;
        return json
            .get("screen_name")
            .and_then(|v| v.as_str())
            .map(str::to_string);
    }
    None
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_operation_and_screen_name() {
        let url = "https://x.com/i/api/graphql/abc123/UserByScreenName?variables=%7B%22screen_name%22%3A%22jack%22%7D&features=%7B%7D";
        assert_eq!(operation_of(url).as_deref(), Some("UserByScreenName"));
        assert_eq!(screen_name_of(url).as_deref(), Some("jack"));
    }

    #[test]
    fn percent_decode_handles_plus_and_non_ascii() {
        assert_eq!(percent_decode("a+b"), "a b");
        assert_eq!(percent_decode("%E4%B8%AD"), "中");
        // 残缺的转义不能 panic
        assert_eq!(percent_decode("100%"), "100%");
    }

    #[test]
    fn missing_screen_name_is_none_not_panic() {
        assert_eq!(
            screen_name_of("https://x.com/i/api/graphql/x/UserByScreenName"),
            None
        );
        assert_eq!(screen_name_of("https://x.com/?features=%7B%7D"), None);
    }

    fn fixture(scenario: &str, m: FixtureMatch) -> Loaded {
        Loaded {
            path: PathBuf::from(format!("{scenario}.json")),
            fixture: Fixture {
                endpoint: "user_by_screen_name".into(),
                scenario: scenario.into(),
                captured_at: None,
                note: None,
                match_: m,
                response: FixtureResponse {
                    status: 200,
                    headers: BTreeMap::new(),
                    body: serde_json::json!({ "ok": true }),
                },
            },
        }
    }

    /// 具体度优先：带 screen_name 的那条必须赢过只有 operation 的那条。
    #[test]
    fn most_specific_fixture_wins() {
        let t = ReplayTransport {
            fixtures: Arc::new(vec![
                fixture(
                    "generic",
                    FixtureMatch {
                        operation: Some("UserByScreenName".into()),
                        ..Default::default()
                    },
                ),
                fixture(
                    "specific",
                    FixtureMatch {
                        operation: Some("UserByScreenName".into()),
                        screen_name: Some("jack".into()),
                        ..Default::default()
                    },
                ),
            ]),
            root: PathBuf::from("."),
        };
        let req = HttpRequest::new(
            HttpMethod::Get,
            "https://x.com/i/api/graphql/abc/UserByScreenName?variables=%7B%22screen_name%22%3A%22jack%22%7D",
        );
        assert_eq!(t.pick(&req).unwrap().fixture.scenario, "specific");
    }

    #[test]
    fn cursor_presence_distinguishes_pages() {
        let t = ReplayTransport {
            fixtures: Arc::new(vec![
                fixture(
                    "page1",
                    FixtureMatch {
                        operation: Some("UserMedia".into()),
                        cursor: Some(CursorMatch::Absent),
                        ..Default::default()
                    },
                ),
                fixture(
                    "page2",
                    FixtureMatch {
                        operation: Some("UserMedia".into()),
                        cursor: Some(CursorMatch::Present),
                        ..Default::default()
                    },
                ),
            ]),
            root: PathBuf::from("."),
        };
        let first_page = HttpRequest::new(
            HttpMethod::Get,
            "https://x.com/i/api/graphql/q/UserMedia?variables=%7B%22userId%22%3A%221%22%7D",
        );
        assert_eq!(
            t.pick(&first_page).unwrap().fixture.scenario,
            "page1",
            "没带 cursor 应命中首页"
        );

        let second_page = HttpRequest::new(
            HttpMethod::Get,
            "https://x.com/i/api/graphql/q/UserMedia?variables=%7B%22userId%22%3A%221%22%2C%22cursor%22%3A%22CUR%22%7D",
        );
        assert_eq!(
            t.pick(&second_page).unwrap().fixture.scenario,
            "page2",
            "带 cursor 应命中翻页样本"
        );
    }

    #[test]
    fn cursor_state_is_read_from_query_or_body() {
        let get = HttpRequest::new(
            HttpMethod::Get,
            "https://x/y?variables=%7B%22userId%22%3A%221%22%7D",
        );
        assert_eq!(cursor_state_of(&get), Some(false));
        let get = HttpRequest::new(
            HttpMethod::Get,
            "https://x/y?variables=%7B%22userId%22%3A%221%22%2C%22cursor%22%3A%22c%22%7D",
        );
        assert_eq!(cursor_state_of(&get), Some(true));

        // POST：cursor 在 body 的 variables 里（搜索端点）
        let post = HttpRequest::new(HttpMethod::Post, "https://x/y")
            .json_body(&serde_json::json!({ "variables": { "rawQuery": "a", "cursor": "c" } }));
        assert_eq!(cursor_state_of(&post), Some(true));
        let post = HttpRequest::new(HttpMethod::Post, "https://x/y")
            .json_body(&serde_json::json!({ "variables": { "rawQuery": "a" } }));
        assert_eq!(cursor_state_of(&post), Some(false));

        // 问不出（没有 variables）→ None，而不是 false
        let bare = HttpRequest::new(HttpMethod::Get, "https://x.com/tesla");
        assert_eq!(cursor_state_of(&bare), None);
    }

    #[test]
    fn auth_none_fixture_requires_absent_cookie() {
        let t = ReplayTransport {
            fixtures: Arc::new(vec![fixture(
                "unauthorized",
                FixtureMatch {
                    auth: Some(AuthMatch::None),
                    ..Default::default()
                },
            )]),
            root: PathBuf::from("."),
        };
        let bare = HttpRequest::new(
            HttpMethod::Get,
            "https://x.com/i/api/graphql/a/UserByScreenName",
        );
        assert!(t.pick(&bare).is_some());
        let with_cookie = bare.clone().header("cookie", "auth_token=x; ct0=y");
        assert!(t.pick(&with_cookie).is_none());
    }

    #[tokio::test]
    async fn unmatched_request_is_a_loud_fixture_error() {
        let t = ReplayTransport {
            fixtures: Arc::new(vec![]),
            root: PathBuf::from("/tmp/nowhere"),
        };
        let req = HttpRequest::new(
            HttpMethod::Get,
            "https://x.com/i/api/graphql/a/UserByScreenName",
        );
        let err = t.execute(&req).await.unwrap_err();
        assert_eq!(err.kind, "fixture");
        // 失败信息要能直接定位到端点，而不是只说 "no fixture"
        assert!(err.message.contains("UserByScreenName"), "{}", err.message);
    }
}
