//! `HttpStack`：内核对外的那一个入口。组件不再各自组装请求头、不再各自限流。
//!
//! 一次调用要穿过这些东西（顺序是刻意的）：
//!
//! ```text
//! 取消检查 → 限流闸门（可能等待/快速失败）→ 签名（x-client-transaction-id）
//!   → 组装头 → 传输 → 状态分类 → 成功复位 / 429 记冷却
//! ```
//!
//! 三条不许破坏的性质：
//! 1. **任何新增请求路径都必须过这里**（铁律 6）；绕过它就等于绕过了限流；
//! 2. **凭据只进不出**：cookie 只出现在请求头里，不进日志、不进错误、不回传；
//! 3. **429 不重试**：重试限流响应只会把限流拖长（因此这里连一次都不重试，
//!    直接记冷却并返回 `rate_limited`，由调用方决定怎么挂起）。

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::cancel::CancelToken;
use crate::creds::Credentials;
use crate::error::{XError, XResult};
use crate::fixture::ReplayTransport;
use crate::http::{ProxyConfig, ReqwestTransport};
use crate::ratelimit::{GateStatus, Limits, RateGate, RequestClass};
use crate::transport::{HttpMethod, HttpRequest, HttpResponse, Transport};
use crate::xclid::{FetchFn, HttpRequestSpec, Signer, BEARER, DEFAULT_PROBE_PATH, USER_AGENT};

/// 单次尝试的尝试次数与总预算上限（`docs/05-WORKFLOW.md` §6 的"不要加载到永远"）。
const MAX_ATTEMPTS: u32 = 3;
const TOTAL_BUDGET: Duration = Duration::from_secs(25);
const INITIAL_BACKOFF: Duration = Duration::from_millis(120);
const MAX_BACKOFF: Duration = Duration::from_secs(8);
/// 抓公开资源（页面 / bundle）的重试次数。它们不在业务关键路径上，
/// 但**在自愈关键路径上**——偶发的连接抖动不该让 queryId 自愈失败。
const TEXT_FETCH_ATTEMPTS: u32 = 3;

/// 一次出网的完整描述。
///
/// 用结构体而不是一路加参数：参数表长到 6 个以后，调用点读起来就是在猜哪个是哪个
/// （`None, true, &cancel` 这种），而这里每个字段的**语义差别**（逻辑端点名 vs URL 路径、
/// 缺省省略 vs 传 null）都值得写出名字。
#[derive(Debug, Clone)]
pub struct ApiSpec<'a> {
    /// **逻辑**端点名（如 `user_media`），只用于错误上下文与日志——不是 URL 路径。
    pub endpoint: &'static str,
    pub method: HttpMethod,
    /// `/i/api/graphql/<queryId>/<Operation>`。queryId 属于组件实现，不进契约。
    pub path: &'a str,
    /// 查询参数；**cursor 缺省时必须整个键省略**（`docs/02` §A1）。
    pub query: &'a [(String, String)],
    /// POST 的 JSON body（搜索端点用；`docs/02` §A2 要求它的形状与 GET 不同）。
    pub body: Option<serde_json::Value>,
    /// `application/x-www-form-urlencoded` 的 body（v1.1 REST 用：关注/取关）。
    ///
    /// 与 `body` 互斥——两种 Content-Type 不能同时出现，谁非空就用谁。
    pub form: Option<&'a [(String, String)]>,
    pub with_credentials: bool,
    /// 目标主机。默认 [`API_HOST`]（x.com）；v1.1 的 friendships 系列必须走
    /// `api.twitter.com`（x.com 域名对该端点 401，参考实现里带着实测注释）。
    pub host: &'static str,
}

/// 取数与页面抓取的主机。
pub const API_HOST: &str = "https://x.com";

/// v1.1 REST 的主机。**只有 friendships 系列用它**，GraphQL 一律走 [`API_HOST`]。
pub const REST_V1_HOST: &str = "https://api.twitter.com";

struct Inner {
    transport: RwLock<Transport>,
    proxy: RwLock<ProxyConfig>,
    gate: RateGate,
    creds: RwLock<Option<Credentials>>,
    signer: Signer,
    probe_path: RwLock<String>,
}

/// 共享内核的 HTTP 栈。克隆是廉价的（内部 `Arc`）。
#[derive(Clone)]
pub struct HttpStack {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for HttpStack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpStack")
            .field("transport", &self.transport().kind())
            .field("authenticated", &self.has_credentials())
            .finish()
    }
}

impl HttpStack {
    fn with_transport(transport: Transport, proxy: ProxyConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                transport: RwLock::new(transport),
                proxy: RwLock::new(proxy),
                gate: RateGate::default(),
                creds: RwLock::new(None),
                signer: Signer::new(),
                probe_path: RwLock::new(DEFAULT_PROBE_PATH.to_string()),
            }),
        }
    }

    /// 真网络。
    pub fn live(proxy: ProxyConfig) -> XResult<Self> {
        let transport = Transport::Live(Box::new(ReqwestTransport::new(&proxy)?));
        Ok(Self::with_transport(transport, proxy))
    }

    /// fixture 回放（离线测试 / `xspiderd --fixture-dir`）。
    pub fn replay(dir: impl AsRef<std::path::Path>) -> XResult<Self> {
        let transport = Transport::Replay(Box::new(ReplayTransport::load(dir)?));
        Ok(Self::with_transport(transport, ProxyConfig::Off))
    }

    pub fn transport(&self) -> Transport {
        self.inner
            .transport
            .read()
            .expect("transport rwlock poisoned")
            .clone()
    }

    pub fn is_replay(&self) -> bool {
        matches!(*self.inner.transport.read().unwrap(), Transport::Replay(_))
    }

    pub fn proxy(&self) -> ProxyConfig {
        self.inner
            .proxy
            .read()
            .expect("proxy rwlock poisoned")
            .clone()
    }

    /// 切换代理。回放模式下的调用被忽略（离线测试不该被代理配置影响）。
    pub fn set_proxy(&self, proxy: ProxyConfig) -> XResult<()> {
        if self.is_replay() {
            tracing::debug!("回放模式下忽略 net.set_proxy");
            return Ok(());
        }
        let transport = Transport::Live(Box::new(ReqwestTransport::new(&proxy)?));
        *self
            .inner
            .transport
            .write()
            .expect("transport rwlock poisoned") = transport;
        *self.inner.proxy.write().expect("proxy rwlock poisoned") = proxy;
        // 代理换了 → 旧的签名密钥未必还适用（不同的出口 IP）；重新握手代价很低
        self.inner.signer.invalidate();
        Ok(())
    }

    pub fn set_credentials(&self, creds: Option<Credentials>) {
        *self.inner.creds.write().expect("creds rwlock poisoned") = creds;
        // cookie 变了 → 登录态页面变了 → 密钥必须重取
        self.inner.signer.invalidate();
    }

    pub fn has_credentials(&self) -> bool {
        self.inner
            .creds
            .read()
            .expect("creds rwlock poisoned")
            .is_some()
    }

    pub fn limits(&self) -> Limits {
        self.inner.gate.limits()
    }

    pub fn set_limits(&self, limits: Limits) {
        self.inner.gate.set_limits(limits);
    }

    pub fn status(&self) -> GateStatus {
        self.inner.gate.status()
    }

    pub fn signer(&self) -> &Signer {
        &self.inner.signer
    }

    fn creds_snapshot(&self) -> Option<Credentials> {
        self.inner
            .creds
            .read()
            .expect("creds rwlock poisoned")
            .clone()
    }

    // ------------------------------------------------------------------
    // 取数：经 `/i/api/` 的 GraphQL 调用
    // ------------------------------------------------------------------

    /// 发一个 GraphQL GET，返回解析后的 JSON。
    ///
    /// - `endpoint`：**逻辑**端点名，只用于错误上下文与日志（不是 URL、不含 queryId）；
    /// - `path`：`/i/api/graphql/<queryId>/<Operation>`（queryId 属于组件实现，不进契约）；
    /// - `query`：查询参数（会被 URL 编码），**cursor 缺省时必须整个键省略**（docs/02 §A1）。
    pub async fn graphql_get(
        &self,
        endpoint: &'static str,
        path: &str,
        query: &[(String, String)],
        cancel: &CancelToken,
    ) -> XResult<Value> {
        let resp = self
            .request_raw(endpoint, HttpMethod::Get, path, query, true, cancel)
            .await?;
        parse_json_body(endpoint, &resp)
    }

    /// 底层请求：含限流、签名、重试预算与 401 自愈。
    pub async fn request_raw(
        &self,
        endpoint: &'static str,
        method: HttpMethod,
        path: &str,
        query: &[(String, String)],
        with_credentials: bool,
        cancel: &CancelToken,
    ) -> XResult<HttpResponse> {
        self.send(
            ApiSpec {
                endpoint,
                method,
                path,
                query,
                body: None,
                form: None,
                with_credentials,
                host: API_HOST,
            },
            cancel,
        )
        .await
    }

    /// POST + JSON body 的 GraphQL 调用（搜索端点专用，`docs/02` §A2）。
    pub async fn graphql_post(
        &self,
        endpoint: &'static str,
        path: &str,
        body: &serde_json::Value,
        cancel: &CancelToken,
    ) -> XResult<Value> {
        let resp = self
            .send(
                ApiSpec {
                    endpoint,
                    method: HttpMethod::Post,
                    path,
                    query: &[],
                    body: Some(body.clone()),
                    form: None,
                    with_credentials: true,
                    host: API_HOST,
                },
                cancel,
            )
            .await?;
        parse_json_body(endpoint, &resp)
    }

    /// **突变**：POST，`variables` 放在 **query string**、不带 body。
    ///
    /// 这是参考实现（`x-spider-mac` 的 `TwitterAPI.mutate`）的形状，**照抄**——
    /// 搜索端点那种"POST + JSON body"是另一回事（`docs/02` §A2），
    /// 两者混用会得到 404/400，而且原因很难猜。
    pub async fn graphql_mutate(
        &self,
        endpoint: &'static str,
        path: &str,
        variables: &serde_json::Value,
        cancel: &CancelToken,
    ) -> XResult<Value> {
        let vars = variables.to_string();
        let resp = self
            .request_raw(
                endpoint,
                HttpMethod::Post,
                path,
                &[("variables".to_string(), vars)],
                true,
                cancel,
            )
            .await?;
        parse_json_body(endpoint, &resp)
    }

    /// v1.1 REST（`api.twitter.com`）的 GET。**关注态查询**用它。
    pub async fn rest_v1_get(
        &self,
        endpoint: &'static str,
        path: &str,
        query: &[(String, String)],
        cancel: &CancelToken,
    ) -> XResult<Value> {
        let resp = self
            .send(
                ApiSpec {
                    endpoint,
                    method: HttpMethod::Get,
                    path,
                    query,
                    body: None,
                    form: None,
                    with_credentials: true,
                    host: REST_V1_HOST,
                },
                cancel,
            )
            .await?;
        parse_json_body(endpoint, &resp)
    }

    /// v1.1 REST 的 form-urlencoded POST。**关注 / 取关**用它。
    pub async fn rest_v1_form_post(
        &self,
        endpoint: &'static str,
        path: &str,
        fields: &[(String, String)],
        cancel: &CancelToken,
    ) -> XResult<Value> {
        let resp = self
            .send(
                ApiSpec {
                    endpoint,
                    method: HttpMethod::Post,
                    path,
                    query: &[],
                    body: None,
                    form: Some(fields),
                    with_credentials: true,
                    host: REST_V1_HOST,
                },
                cancel,
            )
            .await?;
        parse_json_body(endpoint, &resp)
    }

    /// 统一的出网入口：限流 → 签名 → 组装头 → 传输 → 分类 → 重试预算。
    ///
    /// **所有新增请求路径都必须走这里**（铁律 6）；绕过它等于绕过了限流闸门。
    pub async fn send(&self, spec: ApiSpec<'_>, cancel: &CancelToken) -> XResult<HttpResponse> {
        let ApiSpec {
            endpoint,
            method,
            path,
            query,
            body,
            form,
            with_credentials,
            host,
        } = spec;
        if with_credentials && !self.has_credentials() {
            // 没有凭据就不发请求：既省配额，也避免把 401 当成"端点坏了"去排查
            return Err(
                XError::unauthorized("尚未注入凭据：请先调用 auth.set_cookie")
                    .with_endpoint(endpoint),
            );
        }

        let url = build_url(host, path, query);
        let started = Instant::now();
        let mut backoff = INITIAL_BACKOFF;
        let mut attempt = 0u32;
        let mut healed_signer = false;

        loop {
            attempt += 1;
            if cancel.is_cancelled() {
                return Err(XError::Cancelled.with_endpoint(endpoint));
            }
            if started.elapsed() > TOTAL_BUDGET {
                return Err(
                    XError::transport("timeout", "重试总预算（25s）已用尽").with_endpoint(endpoint)
                );
            }

            self.inner
                .gate
                .acquire_or_wait(RequestClass::Api, Some(endpoint), cancel)
                .await?;

            if with_credentials {
                self.ensure_signer(cancel).await?;
            }
            let headers = self.api_headers(method, path, with_credentials)?;

            let mut req = HttpRequest::new(method, &url);
            for (k, v) in headers {
                req = req.header(k, v);
            }
            if let Some(body) = &body {
                req = req.json_body(body);
            } else if let Some(fields) = form {
                // v1.1 REST：form-urlencoded（参考实现 formPost 的形状）
                req = req
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .raw_body(encode_form(fields));
            }

            let outcome = cancel.race(self.transport().execute(&req)).await;
            let resp = match outcome {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => {
                    // 传输层失败：这是唯一值得重试的一类（429 反而不能重试）
                    tracing::warn!(endpoint, kind = e.kind, attempt, "传输失败：{}", e.message);
                    if attempt >= MAX_ATTEMPTS {
                        return Err(XError::from(e).with_endpoint(endpoint));
                    }
                    match cancel.race(tokio::time::sleep(backoff)).await {
                        Ok(()) => {}
                        Err(c) => return Err(c.with_endpoint(endpoint)),
                    }
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                    continue;
                }
                Err(c) => return Err(c.with_endpoint(endpoint)),
            };

            match resp.status {
                200..=299 => {
                    self.inner.gate.note_success(RequestClass::Api);
                    return Ok(resp);
                }
                429 => {
                    let retry_after = resp.retry_after_s();
                    let secs = self
                        .inner
                        .gate
                        .note_rate_limited(RequestClass::Api, retry_after);
                    tracing::warn!(endpoint, retry_after_s = secs, "429：进入冷却，不再重试");
                    return Err(XError::RateLimited {
                        retry_after_s: secs,
                        endpoint: Some(endpoint.to_string()),
                    });
                }
                401 | 403 => {
                    // 两种可能：cookie 真失效，或 transaction id 过期/算法失效。
                    // 无法用结构化字段区分（都是 401 code 89），所以先自愈一次：
                    // 重载密钥再试一遍；仍然 401 就判为凭据问题。
                    // 若 cookie 真失效，重载密钥会在探测页就快速失败并给出 unauthorized，
                    // 不会白等。
                    if with_credentials && !healed_signer {
                        healed_signer = true;
                        tracing::warn!(
                            endpoint,
                            status = resp.status,
                            "疑似签名过期，重载 xclid 后重试一次"
                        );
                        self.inner.signer.invalidate();
                        continue;
                    }
                    return Err(XError::Unauthorized {
                        message: format!("上游返回 HTTP {}：凭据无效或已登出", resp.status),
                        endpoint: Some(endpoint.to_string()),
                    });
                }
                404 => {
                    return Err(XError::Upstream {
                        status: 404,
                        message: "上游返回 404。注意：这**不一定**是 queryId 失效——\
                                  搜索端点用 GET 也一律 404（docs/02 §A2）。"
                            .to_string(),
                        endpoint: Some(endpoint.to_string()),
                    });
                }
                status => {
                    return Err(XError::Upstream {
                        status,
                        message: format!("上游返回 HTTP {status}"),
                        endpoint: Some(endpoint.to_string()),
                    });
                }
            }
        }
    }

    /// 探测型请求：**无论状态码都返回原始响应**，不重试、不分类。
    ///
    /// 与 [`Self::request_raw`] 的分工：
    /// - `request_raw` 面向**业务调用**——4xx/5xx 一律变成结构化错误，
    ///   因为调用方需要的是"能不能用"，不是"上游回了个什么"；
    /// - `probe_raw` 面向**探测与录制**——我们要看的就是那个 403 响应体本身
    ///   （"未登录时上游返回什么"是外壳决定是否提示重新登录的依据，
    ///   也是一条该被真实 fixture 钉住的行为）。
    ///
    /// 仍然走限流闸门（铁律 6：任何请求路径都得过闸门），但不重试、不签名。
    pub async fn probe_raw(
        &self,
        endpoint: &'static str,
        method: HttpMethod,
        path: &str,
        query: &[(String, String)],
        with_credentials: bool,
        cancel: &CancelToken,
    ) -> XResult<HttpResponse> {
        let url = build_url(API_HOST, path, query);
        self.inner
            .gate
            .acquire_or_wait(RequestClass::Api, Some(endpoint), cancel)
            .await?;
        let mut req = HttpRequest::new(method, &url);
        for (k, v) in self.api_headers(method, path, with_credentials)? {
            req = req.header(k, v);
        }
        cancel
            .race(self.transport().execute(&req))
            .await?
            .map_err(XError::from)
    }

    /// 出网 + **只重试传输层失败**。
    ///
    /// 为什么单独抽出来：实测本机代理会**瞬时拒连**（同一个 URL 前一次成功、后一次
    /// `kind=connect` 失败）。而签名加载（`fetch_for_signer`）与抓公开资源
    /// （`fetch_text_with`）原本都是**单发请求**——一次抖动就能让
    /// 「每个带凭据的请求都要走的签名路径」直接失败，表现为莫名其妙的
    /// "网络错误（connect）"，而重跑一次就好了。
    ///
    /// 纪律与主路径一致：**只重试传输层失败**；拿到 4xx/5xx 一律如实返回，
    /// 重试服务端说的"不行"只会放大问题（`docs/02` §B7）。
    async fn execute_with_retry(
        &self,
        req: &HttpRequest,
        class: RequestClass,
        cancel: &CancelToken,
    ) -> XResult<HttpResponse> {
        self.execute_with_retry_labeled(req, class, "public_asset", cancel)
            .await
    }

    /// 与 [`Self::execute_with_retry`] 相同，但错误里用的逻辑端点名可以指定
    /// ——`public_asset` 这个标签出现在错误信息里，探测媒体大小时用 `media_probe`
    /// 才说得清"是哪条路径失败了"。
    async fn execute_with_retry_labeled(
        &self,
        req: &HttpRequest,
        class: RequestClass,
        endpoint: &'static str,
        cancel: &CancelToken,
    ) -> XResult<HttpResponse> {
        let mut backoff = Duration::from_millis(150);
        let mut last: Option<XError> = None;
        for attempt in 1..=TEXT_FETCH_ATTEMPTS {
            self.inner
                .gate
                .acquire_or_wait(class, Some(endpoint), cancel)
                .await?;
            match cancel.race(self.transport().execute(req)).await {
                Ok(Ok(resp)) => return Ok(resp),
                Ok(Err(e)) => {
                    tracing::debug!(attempt, kind = e.kind, "出网失败，将重试");
                    last = Some(XError::from(e));
                }
                Err(cancelled) => return Err(cancelled),
            }
            if attempt < TEXT_FETCH_ATTEMPTS {
                match cancel.race(tokio::time::sleep(backoff)).await {
                    Ok(()) => {}
                    Err(cancelled) => return Err(cancelled),
                }
                backoff = (backoff * 3).min(MAX_BACKOFF);
            }
        }
        Err(last.unwrap_or_else(|| XError::transport("other", "出网失败")))
    }

    /// 抓一段文本（HTML 页面 / JS bundle / 脚本）。
    ///
    /// `with_credentials` 由调用方决定，**默认应该是 false**（公开资源不需要 cookie）；
    /// 但有一个必须为 true 的场景：queryId 自愈要读 `/search` 页面，而
    /// **未登录访问 `/search` 会被 307 到 onboarding 页面，那个页面里没有任何 bundle 引用**
    /// （实测：未登录 17KB 无脚本；带 cookie 307KB 且含 `main.<hash>.js`）。
    ///
    /// 这类请求的大头在 `abs.twimg.com`（CDN），所以按 **CDN 配额**过闸门，
    /// 不去挤接口配额（`docs/02` §B6：两套配额分开治理）。
    pub async fn fetch_text_with(
        &self,
        url: &str,
        with_credentials: bool,
        class: RequestClass,
        cancel: &CancelToken,
    ) -> XResult<String> {
        // 公开页面会 307 到登录页，所以这类抓取要跟随重定向
        let mut req = HttpRequest::new(HttpMethod::Get, url)
            .header("User-Agent", USER_AGENT)
            .header("Referer", API_HOST)
            .follow_redirects();
        if with_credentials {
            let creds = self
                .creds_snapshot()
                .ok_or_else(|| XError::unauthorized("尚未注入凭据"))?;
            req = req.header("Cookie", creds.cookie().to_string());
        }

        let resp = self.execute_with_retry(&req, class, cancel).await?;
        if !(200..300).contains(&resp.status) {
            return Err(XError::Upstream {
                status: resp.status,
                message: format!("抓取资源失败：{url}"),
                endpoint: Some("public_asset".to_string()),
            });
        }
        Ok(resp.body_text())
    }

    /// 问服务端"这个文件多大"，**不下载任何字节**（或只下 1 字节）。
    ///
    /// # 为什么必须联网问
    ///
    /// GraphQL 的 media 对象里**没有字节数**（只有宽高、时长、码率），
    /// 而 `码率 × 时长` 是编码器上限、不是实际大小——实测差 5.25 倍
    /// （`docs/02` §E9）。CDN 则直接给精确值：`HEAD` 的 `Content-Length`，
    /// 不支持时退到 `Range: bytes=0-0` 的 `Content-Range: bytes 0-0/<总长>`。
    ///
    /// # 三条纪律
    ///
    /// 1. **走 CDN 配额过闸门**（`docs/02` §B6）：探大小也是流量，不该挤接口配额；
    /// 2. **回放模式直接返回 `Ok(None)`**：离线测试不碰网络（铁律 3），
    ///    而"服务端没说"本来就是这个方法的合法返回值；
    /// 3. **带 UA 与 Referer、且 `Accept-Encoding: identity`**：与真实下载用同一套头，
    ///    否则 `Content-Length` 可能是被压缩后的大小，与实际落盘字节数对不上，
    ///    完整性校验会**假红**。
    ///
    /// 返回 `Ok(None)` = "服务端没说"，不是错误；404 则如实报 `not_found`
    /// （对调用方有意义：这个媒体确实不在了，别下）。
    pub async fn probe_size(&self, url: &str, cancel: &CancelToken) -> XResult<Option<u64>> {
        if self.is_replay() {
            tracing::debug!("回放模式：不探测媒体大小（要联网），一律按未知处理");
            return Ok(None);
        }

        // 先试 HEAD：最省，一个字节都不下
        let head = HttpRequest::new(HttpMethod::Head, url)
            .header("User-Agent", USER_AGENT)
            .header("Referer", API_HOST)
            .header("Accept-Encoding", "identity");
        let resp = self
            .execute_with_retry_labeled(&head, RequestClass::Cdn, "media_probe", cancel)
            .await?;
        if resp.is_success() {
            if let Some(size) = resp.header("content-length").and_then(parse_u64) {
                return Ok(Some(size));
            }
        }

        // HEAD 不被支持（405/501）或没给长度 → 用 1 字节的 Range 问总长
        let range = HttpRequest::new(HttpMethod::Get, url)
            .header("User-Agent", USER_AGENT)
            .header("Referer", API_HOST)
            .header("Accept-Encoding", "identity")
            .header("Range", "bytes=0-0");
        let resp = self
            .execute_with_retry_labeled(&range, RequestClass::Cdn, "media_probe", cancel)
            .await?;
        if resp.status == 404 {
            return Err(XError::not_found("媒体不存在（HTTP 404）").with_endpoint("media_probe"));
        }
        // 206 → Content-Range 里带总长；200 → 服务端忽略了 Range，这时的
        // Content-Length 就是整个文件的大小
        if let Some(total) = resp
            .header("content-range")
            .and_then(|value| value.rsplit('/').next())
            .and_then(|total| total.trim().parse::<u64>().ok())
        {
            return Ok(Some(total));
        }
        Ok(resp.header("content-length").and_then(parse_u64))
    }

    /// 组装 `/i/api/` 请求头。头集合要与上游一致（docs/02 §A5）：少一个就可能 403 或空数据。
    fn api_headers(
        &self,
        method: HttpMethod,
        path: &str,
        with_credentials: bool,
    ) -> XResult<Vec<(String, String)>> {
        let mut headers: Vec<(String, String)> = vec![
            ("User-Agent".into(), USER_AGENT.into()),
            ("Referer".into(), API_HOST.into()),
        ];
        if !with_credentials {
            return Ok(headers);
        }
        let creds = self
            .creds_snapshot()
            .ok_or_else(|| XError::unauthorized("尚未注入凭据"))?;
        headers.push(("Authorization".into(), BEARER.into()));
        headers.push(("Cookie".into(), creds.cookie().to_string()));
        headers.push(("X-Csrf-Token".into(), creds.csrf().to_string()));
        headers.push(("x-twitter-active-user".into(), "yes".into()));
        headers.push(("x-twitter-client-language".into(), "en".into()));
        if let Some(txid) = self.inner.signer.transaction_id(method, path) {
            headers.push(("x-client-transaction-id".into(), txid));
        }
        Ok(headers)
    }

    /// 确保签名密钥可用。**这条路径刻意不过限流闸门**：
    /// 每小时握手一次，且它本身是登录态校验（cookie 失效会在这里就快速失败）。
    async fn ensure_signer(&self, cancel: &CancelToken) -> XResult<()> {
        if self.inner.signer.is_loaded() {
            return Ok(());
        }
        if self.is_replay() {
            // fixture 回放模式下**不签名**：签名密钥只能从真实登录态页面取，
            // 离线环境里没有会话可签，硬要走网络就违背了"默认离线测试"（铁律 3）。
            //
            // 于是离线测试不覆盖签名头本身——它由 live recorder 与 canary 覆盖
            // （`XSPIDER_LIVE=1`，见 crates/xspider-fetch/tests/contract_dual.rs 的说明）。
            // 这个取舍是明确的：离线测试要在意的解析、分页、错误分类，
            // 都不经过那条路径。
            tracing::debug!("回放模式：跳过请求签名");
            return Ok(());
        }
        let probe_path = self
            .inner
            .probe_path
            .read()
            .expect("probe_path rwlock poisoned")
            .clone();
        let this = self.clone();
        let cancel = cancel.clone();
        let fetch: Arc<FetchFn> = Arc::new(move |spec: HttpRequestSpec| {
            let this = this.clone();
            let cancel = cancel.clone();
            Box::pin(async move { this.fetch_for_signer(&spec, &cancel).await })
        });
        self.inner.signer.ensure_loaded(fetch, &probe_path).await
    }

    /// 签名加载期间的请求（探测页 + 脚本），带超时与取消。
    ///
    /// `pub(crate)` 是为了让录制 fixture 的测试走**同一条**路径，
    /// 而不是在测试里另写一份取数逻辑（那样录出来的东西不代表生产行为）。
    pub(crate) async fn fetch_for_signer(
        &self,
        spec: &HttpRequestSpec,
        cancel: &CancelToken,
    ) -> XResult<HttpResponse> {
        let mut req = HttpRequest::new(spec.method, &spec.url)
            .header("User-Agent", USER_AGENT)
            .header("Referer", API_HOST);
        if spec.with_credentials {
            let creds = self
                .creds_snapshot()
                .ok_or_else(|| XError::unauthorized("尚未注入凭据"))?;
            req = req.header("Cookie", creds.cookie().to_string());
        }
        // 签名加载在**每个带凭据请求的关键路径**上，所以同样要能吃住代理的瞬时抖动；
        // 这类请求是页面/脚本，按 CDN 配额过闸门（与 fetch_text_with 一致）
        self.execute_with_retry(&req, RequestClass::Cdn, cancel)
            .await
    }
}

fn build_url(host: &str, path: &str, query: &[(String, String)]) -> String {
    let mut url = format!("{host}{path}");
    if !query.is_empty() {
        url.push('?');
        let encoded: Vec<String> = query
            .iter()
            .map(|(k, v)| format!("{k}={}", crate::url::encode_query_value(v)))
            .collect();
        url.push_str(&encoded.join("&"));
    }
    url
}

/// `application/x-www-form-urlencoded` 的编码（v1.1 REST 的 body）。
///
/// 只转义真正需要转义的字符：与参考实现一致（它用 `urlQueryAllowed`，
/// 连 `&` 与 `=` 都放过去——照抄，别顺手"修好"，X 认的是它现在这个形状）。
fn encode_form(fields: &[(String, String)]) -> Vec<u8> {
    fields
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
        .into_bytes()
}

/// 头值里的十进制整数（`Content-Length` 之类）。
fn parse_u64(raw: &str) -> Option<u64> {
    raw.trim().parse::<u64>().ok()
}

/// 解析响应体为 JSON，失败时给出**能定位到字段**的错误（docs/05 §6「错误里带上下文」）。
fn parse_json_body(endpoint: &'static str, resp: &HttpResponse) -> XResult<Value> {
    let text = resp.body_text();
    if text.trim().is_empty() {
        return Err(XError::Parse {
            context: format!("{endpoint}.body"),
            message: "响应体为空".into(),
            endpoint: Some(endpoint.to_string()),
        });
    }
    serde_json::from_str::<Value>(&text).map_err(|e| XError::Parse {
        context: format!("{endpoint}.body"),
        message: format!(
            "响应体不是合法 JSON（{e}）；前 200 字符：{}",
            preview(&text)
        ),
        endpoint: Some(endpoint.to_string()),
    })
}

fn preview(text: &str) -> String {
    text.chars().take(200).collect()
}
