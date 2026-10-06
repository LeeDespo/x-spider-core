//! 真网络的 HTTP 传输实现。
//!
//! 两个选型是硬性的（会拖死交叉编译，见 `docs/03-FFI-SIGNING-PACKAGING.md` §6）：
//! - **rustls，不用 native-tls**：否则 Windows/Linux 交叉编译要处理 OpenSSL；
//! - 无 C 工具链依赖。
//!
//! 另外两处是实测教训：
//! - **不用 cookie store**：macOS 的共享 Cookie 存储会自动注入 guest cookie，
//!   覆盖请求头里手写的 `auth_token`，表现为莫名其妙的 401；
//! - **不跟随重定向**：X 的 GraphQL 不该有重定向，跟随只会把 401 变成一次诡异的 200。

use std::time::Duration;

use crate::error::{XError, XResult};
use crate::transport::{HttpRequest, HttpResponse, TransportError, TransportResult};

/// 代理三态。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ProxyConfig {
    /// 跟随标准环境变量（`HTTPS_PROXY` / `ALL_PROXY` / `NO_PROXY`）。
    #[default]
    Env,
    /// 显式关闭代理（忽略环境变量）。
    Off,
    /// 指定代理 URL，例如 `http://127.0.0.1:17890`。
    Manual(String),
}

impl ProxyConfig {
    /// 契约方法 `net.set_proxy` 的入参解析：`null` → 关闭；字符串 → 指定；缺省 → 环境变量。
    pub fn from_contract(value: Option<&serde_json::Value>) -> XResult<Self> {
        match value {
            None | Some(serde_json::Value::Null) => Ok(ProxyConfig::Off),
            Some(serde_json::Value::String(s)) if !s.trim().is_empty() => {
                Ok(ProxyConfig::Manual(s.trim().to_string()))
            }
            Some(serde_json::Value::String(_)) => Err(XError::invalid_request(
                "net.set_proxy 的 url 不能是空字符串（要关闭代理请传 null）",
            )),
            Some(_) => Err(XError::invalid_request(
                "net.set_proxy 的 url 必须是字符串或 null",
            )),
        }
    }

    /// 解析成一个**具体的**代理 URL。
    ///
    /// `reqwest` 自己会读环境变量，所以进程内的 HTTP 用不上这个；
    /// 但**子进程（Aria2Next）不吃"跟随环境变量"这个概念**——它要么拿到一个 URL，
    /// 要么走直连。没有这个方法，外派后端就会在"需要代理才能出去"的机器上静默失败。
    ///
    /// 环境变量顺序与 reqwest 的习惯一致（大写优先，其次小写）。
    pub fn resolve_url(&self) -> Option<String> {
        match self {
            ProxyConfig::Off => None,
            ProxyConfig::Manual(url) => Some(url.clone()),
            ProxyConfig::Env => ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"]
                .iter()
                .find_map(|key| std::env::var(key).ok())
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReqwestTransport {
    /// 接口请求用：**不跟随重定向**。
    ///
    /// 跟随会把"鉴权失败（302 到登录页）"伪装成一次看起来成功的请求，
    /// 而那是这条链上最难排查的一类症状。
    client: reqwest::Client,
    /// 公开页面用：跟随重定向。未登录访问 `/search` 会 307 到登录页，
    /// 而登录页同样是 SPA 外壳，里面照样有我们要的 bundle 引用。
    ///
    /// 两个客户端而不是一个：reqwest 0.12 的 `RequestBuilder` **没有** per-request
    /// redirect 覆写，只能在客户端层面分开。
    web_client: reqwest::Client,
}

impl ReqwestTransport {
    pub fn new(proxy: &ProxyConfig) -> XResult<Self> {
        Ok(Self {
            client: build_client(proxy, reqwest::redirect::Policy::none())?,
            web_client: build_client(proxy, reqwest::redirect::Policy::limited(5))?,
        })
    }

    pub async fn execute(&self, req: &HttpRequest) -> TransportResult {
        let method = match req.method {
            crate::transport::HttpMethod::Get => reqwest::Method::GET,
            crate::transport::HttpMethod::Post => reqwest::Method::POST,
            crate::transport::HttpMethod::Head => reqwest::Method::HEAD,
        };
        let client = if req.follow_redirects {
            &self.web_client
        } else {
            &self.client
        };
        let mut builder = client.request(method, &req.url);
        for (k, v) in &req.headers {
            builder = builder.header(k, v);
        }
        if let Some(body) = &req.body {
            builder = builder.body(body.clone());
        }
        let sent = builder.send().await.map_err(|e| map_error(&e))?;

        let status = sent.status().as_u16();
        let mut headers: Vec<(String, String)> = Vec::new();
        for (name, value) in sent.headers() {
            headers.push((
                name.as_str().to_string(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            ));
        }
        let body = sent.bytes().await.map_err(|e| map_error(&e))?.to_vec();
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}

/// 按重定向策略构造客户端；其余配置两边一致（同代理、同超时、同连接池上限）。
fn build_client(
    proxy: &ProxyConfig,
    redirect: reqwest::redirect::Policy,
) -> XResult<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .redirect(redirect)
        .connect_timeout(Duration::from_secs(10))
        // 整体超时；重试与总预算由上层（HttpStack）掌握
        .timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(4);

    builder = match proxy {
        ProxyConfig::Env => builder,
        ProxyConfig::Off => builder.no_proxy(),
        ProxyConfig::Manual(url) => builder.proxy(
            reqwest::Proxy::all(url)
                .map_err(|e| XError::invalid_request(format!("代理 URL 不合法（{url}）：{e}")))?,
        ),
    };

    builder
        .build()
        .map_err(|e| XError::internal(format!("构造 HTTP 客户端失败：{e}")))
}

/// `reqwest::Error` → 结构化 kind。**只用结构化谓词**，不匹配错误文案：
/// 文案是引擎的实现细节，换版本就变了（这正是本项目禁止的脆弱性）。
fn map_error(e: &reqwest::Error) -> TransportError {
    let kind = if e.is_timeout() {
        "timeout"
    } else if e.is_connect() {
        "connect"
    } else if e.is_body() || e.is_decode() {
        "body"
    } else {
        // 其余一律归 other：**不要**为了好看去匹配错误文案来细分类别
        "other"
    };
    TransportError::new(kind, e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn proxy_config_parses_contract_values() {
        assert!(matches!(
            ProxyConfig::from_contract(None).unwrap(),
            ProxyConfig::Off
        ));
        assert!(matches!(
            ProxyConfig::from_contract(Some(&json!(null))).unwrap(),
            ProxyConfig::Off
        ));
        assert_eq!(
            ProxyConfig::from_contract(Some(&json!("http://127.0.0.1:17890"))).unwrap(),
            ProxyConfig::Manual("http://127.0.0.1:17890".into())
        );
        assert!(ProxyConfig::from_contract(Some(&json!(""))).is_err());
        assert!(ProxyConfig::from_contract(Some(&json!(12))).is_err());
    }

    #[test]
    fn client_builds_for_every_proxy_mode() {
        for mode in [
            ProxyConfig::Env,
            ProxyConfig::Off,
            ProxyConfig::Manual("http://127.0.0.1:17890".into()),
        ] {
            ReqwestTransport::new(&mode).expect("每种代理模式都应能构造客户端");
        }
    }

    #[test]
    fn rejects_invalid_proxy_url() {
        let err = ReqwestTransport::new(&ProxyConfig::Manual("not a url".into())).unwrap_err();
        assert_eq!(err.code(), crate::error::ErrorCode::InvalidRequest);
    }

    /// 子进程（Aria2Next）不吃"跟随环境变量"，所以必须能解析出一个具体 URL。
    /// `Env` 分支要读环境变量，测试里不碰它——那种用例在并行测试下会互相污染
    /// （踩坑 6 是同一个教训，见 docs/05-WORKFLOW.md 的踩坑总索引）。
    #[test]
    fn proxy_resolves_to_a_concrete_url() {
        assert_eq!(ProxyConfig::Off.resolve_url(), None);
        assert_eq!(
            ProxyConfig::Manual("http://127.0.0.1:17890".into()).resolve_url(),
            Some("http://127.0.0.1:17890".into())
        );
    }
}
