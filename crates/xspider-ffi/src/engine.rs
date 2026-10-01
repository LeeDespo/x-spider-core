//! method 派发：**双形态行为一致的结构性保证**。
//!
//! `xspider_call`（cdylib）与 `xspiderd`（sidecar）都调用这里的同一个 [`Engine::call`]。
//! 一致性不是靠人工对齐两份实现，而是因为它们本来就是同一份代码
//! （`docs/04-TESTING-AND-FIXTURES.md` §4 的契约测试就是来钉住这件事的）。

use std::sync::Arc;

use serde_json::Value;

use xspider_core::cancel::CancelToken;
use xspider_core::creds::Credentials;
use xspider_core::error::{XError, XResult};
use xspider_core::http::ProxyConfig;
use xspider_core::paging::Page;
use xspider_core::ratelimit::Limits;
use xspider_core::stack::HttpStack;
use xspider_core::{BUILD_VERSION, CONTRACT_VERSION};
use xspider_download::{
    Aria2NextConfig, CrawlLimits, CrawlStrategy, DownloadQueue, EnqueueJob, PageSource, QueueConfig,
};
use xspider_fetch::FetchClient;

/// 本实现支持的全部 method。契约测试会拿它和 `contract/xspider.schema.json` 的
/// `method` 枚举做**集合相等**断言，防止文档和实现对不上（铁律 1：契约优先）。
pub const METHODS: &[&str] = &[
    "system.version",
    "system.methods",
    "auth.set_cookie",
    "auth.whoami",
    "net.set_limits",
    "net.set_proxy",
    "net.status",
    "net.probe_size",
    "fetch.get_user",
    "fetch.user_medias",
    "fetch.user_tweets",
    "fetch.tweet_detail",
    "fetch.search_timeline",
    "fetch.home_timeline",
    "fetch.following",
    "fetch.is_following",
    "fetch.mutate",
    "dl.enqueue",
    "dl.pause",
    "dl.resume",
    "dl.cancel",
    "dl.status",
    "dl.list",
    "dl.events",
    "dl.prune",
    "crawl.run",
];

/// 组件跑在哪种**传输形态**里（`docs/CONTRACT.md` §3.2）。
///
/// 为什么外壳需要知道：两种形态的**能力不完全一样**。`system.shutdown`
/// 只有 sidecar 有（cdylib 没有"关掉宿主进程"这回事），而它不在 `system.methods`
/// 里（方法清单只列契约载荷）。外壳靠这个字段决定"要不要显示退出菜单项 /
/// 要不要自己接管退出"——不必靠"我是不是我"来硬编码（ADR-037）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    /// 独立进程 + 本地 JSON-RPC（主形态）。
    Sidecar,
    /// 进程内的动态库形态。
    Cdylib,
}

impl Transport {
    pub fn as_str(self) -> &'static str {
        match self {
            Transport::Sidecar => "sidecar",
            Transport::Cdylib => "cdylib",
        }
    }
}

#[derive(Clone)]
pub struct Engine {
    stack: HttpStack,
    fetch: Arc<FetchClient>,
    /// 下载队列。**懒初始化**：不用下载能力的外壳不该被它拖起来
    /// （起 Aria2Next 要拉一个子进程，那是有代价的）。
    download: Arc<tokio::sync::OnceCell<Arc<DownloadQueue>>>,
    /// 爬取事件的环形缓冲（与队列的日志同构：三种传输都靠"游标轮询"取增量）。
    crawl_log: Arc<std::sync::Mutex<CrawlEventLog>>,
    /// 当前形态。**默认 `Cdylib`**：直接用 `Engine` 的 Rust API 就是进程内调用；
    /// xspiderd 会显式改成 `Sidecar`（它才知道自己在当 sidecar）。
    transport: Transport,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("stack", &self.stack)
            .finish()
    }
}

impl Engine {
    pub fn with_stack(stack: HttpStack) -> Self {
        let fetch = Arc::new(FetchClient::new(stack.clone()));
        Self {
            stack,
            fetch,
            download: Arc::new(tokio::sync::OnceCell::new()),
            crawl_log: Arc::new(std::sync::Mutex::new(CrawlEventLog::default())),
            transport: Transport::Cdylib,
        }
    }

    /// 声明自己跑在哪种形态里（只有 sidecar 需要显式调用）。
    pub fn with_transport(mut self, transport: Transport) -> Self {
        self.transport = transport;
        self
    }

    pub fn transport(&self) -> Transport {
        self.transport
    }

    /// 取（必要时创建）下载队列。
    ///
    /// 配置来自环境变量，且**引擎选择留在组件内部**（`docs/01` §5.2 纪律 1）：
    /// - `XSPIDER_ARIA2_PATH`：Aria2Next 二进制；给了就启用外派后端；
    /// - `XSPIDER_STATE_DIR`：下载记录（重启对账）落在这里。
    async fn download_queue(&self) -> XResult<Arc<DownloadQueue>> {
        let queue = self
            .download
            .get_or_try_init(|| async {
                let mut config = QueueConfig {
                    proxy: self.stack.proxy(),
                    limits: self.stack.limits(),
                    ..QueueConfig::default()
                };
                if let Some(binary) = Aria2NextConfig::locate_binary() {
                    config.aria2 = Some(Aria2NextConfig::new(binary, std::env::temp_dir()));
                }
                if let Ok(dir) = std::env::var("XSPIDER_STATE_DIR") {
                    if !dir.trim().is_empty() {
                        config.records_path =
                            Some(std::path::PathBuf::from(dir).join("downloads.json"));
                    }
                }
                // 探测媒体大小是**可关**的：它对完整性校验有价值（`docs/02` §E2），
                // 代价是每个未知大小的媒体多一次 CDN 请求。参考实现选择不探
                // （用 5.25 倍误差的估算，`docs/02` §E9）——这里把选择权留给外壳。
                if let Ok(value) = std::env::var("XSPIDER_PROBE_SIZE") {
                    if matches!(value.trim(), "0" | "false" | "off") {
                        config.probe_size_when_unknown = false;
                    }
                }
                DownloadQueue::start(config).await.map(Arc::new)
            })
            .await?;
        Ok(queue.clone())
    }

    /// 真网络引擎。
    pub fn live(proxy: ProxyConfig) -> XResult<Self> {
        Ok(Self::with_stack(HttpStack::live(proxy)?))
    }

    /// fixture 回放引擎（离线测试 / `xspiderd --fixture-dir`）。
    pub fn replay(dir: impl AsRef<std::path::Path>) -> XResult<Self> {
        Ok(Self::with_stack(HttpStack::replay(dir)?))
    }

    pub fn stack(&self) -> &HttpStack {
        &self.stack
    }

    /// 派发一次调用。
    ///
    /// `params` 是 methods 各自的参数对象（契约里 `json_in` 的位置）。
    /// 返回值是**成功时的 `result` 内容**；错误由调用方包成 `{"error": ...}`。
    pub async fn call(&self, method: &str, params: &Value) -> XResult<Value> {
        let cancel = CancelToken::new();
        self.call_with_cancel(method, params, &cancel).await
    }

    pub async fn call_with_cancel(
        &self,
        method: &str,
        params: &Value,
        cancel: &CancelToken,
    ) -> XResult<Value> {
        // 未知 method 必须是**结构化错误**，不是 panic、也不是空字符串
        // （docs/04 §4 明确点名这一条）。
        if !METHODS.contains(&method) {
            return Err(XError::invalid_request(format!(
                "未知 method：{method}（本版本支持：{}）",
                METHODS.join(", ")
            )));
        }
        let params = ensure_object(params)?;

        match method {
            "system.version" => Ok(serde_json::json!({
                "contract_version": CONTRACT_VERSION,
                "build_version": BUILD_VERSION,
                // 能力差异（`system.shutdown` 只有 sidecar 有）靠它表达，
                // 而不是让外壳硬编码"我是 sidecar 所以我能关"（ADR-037）
                "transport": self.transport.as_str(),
            })),
            "system.methods" => Ok(serde_json::json!({ "methods": METHODS })),
            "auth.set_cookie" => self.set_cookie(params),
            // 我是谁：外壳用它做登录校验与"当前账号"展示（原来是抓首页 HTML 自己正则）
            "auth.whoami" => {
                let account = self.fetch.whoami(cancel).await?;
                Ok(serde_json::json!({ "account": account }))
            }
            // 关注态：**这是每张推文卡都会问一次的**（关注按钮），所以组件缓存了"我是谁"
            "fetch.is_following" => {
                let screen_name = required_str(params, "screen_name")?;
                let following = self.fetch.is_following(&screen_name, cancel).await?;
                Ok(serde_json::json!({ "following": following }))
            }
            // 写操作：动的是用户的真实账号，所以参数校验在最前面（缺字段不发请求）
            "fetch.mutate" => {
                let action = required_str(params, "action")?;
                let tweet_id = optional_str(params, "tweet_id")?;
                let screen_name = optional_str(params, "screen_name")?;
                self.fetch
                    .mutate(&action, tweet_id.as_deref(), screen_name.as_deref(), cancel)
                    .await?;
                Ok(serde_json::json!({ "ok": true }))
            }
            "net.set_limits" => self.set_limits(params),
            "net.set_proxy" => self.set_proxy(params),
            "net.status" => Ok(serde_json::to_value(self.stack.status()).unwrap_or(Value::Null)),
            // 问 CDN"这个文件多大"：外壳可以在**决定下不下之前**按大小过滤，
            // 而不是等组件在入队后自己探（那条路是 `dl.enqueue` 的
            // `probe_size_when_unknown`，见 ADR-032）。
            "net.probe_size" => {
                let url = required_str(params, "url")?;
                let size = self.stack.probe_size(&url, cancel).await?;
                Ok(serde_json::json!({ "size": size }))
            }
            "fetch.get_user" => {
                let screen_name = required_str(params, "screen_name")?;
                let user = self.fetch.get_user(&screen_name, cancel).await?;
                Ok(serde_json::json!({ "user": user }))
            }
            "fetch.user_medias" => {
                let user_id = required_str(params, "user_id")?;
                let cursor = optional_str(params, "cursor")?;
                let count = optional_positive_u64(params, "count")?;
                let page = self
                    .fetch
                    .user_medias(&user_id, cursor.as_deref(), count, cancel)
                    .await?;
                Ok(serde_json::to_value(page).unwrap_or(Value::Null))
            }
            "fetch.user_tweets" => {
                let user_id = required_str(params, "user_id")?;
                let cursor = optional_str(params, "cursor")?;
                let count = optional_positive_u64(params, "count")?;
                let require_media = optional_bool(params, "require_media")?;
                let include_retweets = optional_bool(params, "include_retweets")?;
                let page = self
                    .fetch
                    .user_tweets(
                        &user_id,
                        cursor.as_deref(),
                        count,
                        require_media,
                        include_retweets,
                        cancel,
                    )
                    .await?;
                Ok(serde_json::to_value(page).unwrap_or(Value::Null))
            }
            "fetch.tweet_detail" => {
                let id = required_str(params, "id")?;
                let detail = self.fetch.tweet_detail(&id, cancel).await?;
                Ok(serde_json::to_value(detail).unwrap_or(Value::Null))
            }
            "fetch.search_timeline" => {
                let screen_name = required_str(params, "screen_name")?;
                let since = required_str(params, "since")?;
                let until = required_str(params, "until")?;
                // media_only 缺省为 true：这个端点的主要用途就是"取媒体"，
                // 而且服务端筛（filter:media）比取回来再筛省请求也省配额
                let media_only = optional_bool(params, "media_only")?.unwrap_or(true);
                let cursor = optional_str(params, "cursor")?;
                let page = self
                    .fetch
                    .search_timeline(
                        &screen_name,
                        &since,
                        &until,
                        media_only,
                        cursor.as_deref(),
                        cancel,
                    )
                    .await?;
                Ok(serde_json::to_value(page).unwrap_or(Value::Null))
            }
            "fetch.home_timeline" => {
                let mode = required_str(params, "mode")?;
                let cursor = optional_str(params, "cursor")?;
                let page = self
                    .fetch
                    .home_timeline(&mode, cursor.as_deref(), cancel)
                    .await?;
                Ok(serde_json::to_value(page).unwrap_or(Value::Null))
            }
            "fetch.following" => {
                let user_id = required_str(params, "user_id")?;
                let cursor = optional_str(params, "cursor")?;
                let count = optional_positive_u64(params, "count")?;
                let page = self
                    .fetch
                    .following(&user_id, cursor.as_deref(), count, cancel)
                    .await?;
                Ok(serde_json::to_value(page).unwrap_or(Value::Null))
            }
            "dl.enqueue" => {
                let job = EnqueueJob {
                    job_id: xspider_download::JobId::new(required_str(params, "job_id")?),
                    url: required_str(params, "url")?,
                    dest_path: dest_path_from(params)?,
                    expect_size: optional_u64(params, "expect_size")?,
                    requirements: match params.get("requirements") {
                        None | Some(Value::Null) => xspider_download::Requirements::default(),
                        Some(value) => serde_json::from_value(value.clone()).map_err(|e| {
                            XError::invalid_request(format!("requirements 形状不对：{e}"))
                        })?,
                    },
                    tag: optional_str(params, "tag")?,
                    skip_if_present: optional_bool(params, "skip_if_present")?.unwrap_or(false),
                };
                let accepted = self.download_queue().await?.enqueue(job).await?;
                Ok(serde_json::json!({ "accepted_by": accepted }))
            }
            "dl.pause" | "dl.resume" | "dl.cancel" => {
                let job_id = required_str(params, "job_id")?;
                let queue = self.download_queue().await?;
                let action = match method {
                    "dl.pause" => queue.pause(&job_id),
                    "dl.resume" => queue.resume(&job_id),
                    _ => queue.cancel(&job_id),
                };
                action?;
                Ok(serde_json::json!({ "ok": true }))
            }
            "dl.status" => {
                let job_id = required_str(params, "job_id")?;
                let queue = self.download_queue().await?;
                let snapshot = queue
                    .status(&job_id)
                    .ok_or_else(|| XError::not_found(format!("未知 job_id：{job_id}")))?;
                Ok(serde_json::json!({ "job": snapshot }))
            }
            "dl.list" => {
                let queue = self.download_queue().await?;
                Ok(serde_json::json!({ "jobs": queue.list() }))
            }
            "dl.prune" => {
                let queue = self.download_queue().await?;
                queue.prune_finished();
                Ok(serde_json::json!({ "ok": true }))
            }
            "dl.events" => {
                let since = optional_u64(params, "since")?.unwrap_or(0);
                let queue = self.download_queue().await?;
                let (seq, events) = queue.events_since(since);
                Ok(serde_json::json!({ "seq": seq, "events": events }))
            }
            "crawl.run" => self.crawl_run(params, cancel).await,
            // METHODS 里列过就一定被上面的分支覆盖；这里只是让穷尽性显式化
            other => Err(XError::internal(format!("method {other} 已登记但未实现"))),
        }
    }

    /// `crawl.run`：跑一次爬取循环，返回候选清单与 `done_reason`。
    ///
    /// **当前是"跑到停为止再返回"**（受 `limits.max_pages` 约束）。长爬取应当由外壳
    /// 用小页数反复调用、并用返回的 `next_cursor` 续爬——这样进度对调用方可见，
    /// 也不必把"长任务"塞进一次请求里（契约里 `crawl.*` 的事件流是 M2 之后的活）。
    async fn crawl_run(
        &self,
        params: &serde_json::Map<String, Value>,
        cancel: &CancelToken,
    ) -> XResult<Value> {
        let source = required_str(params, "source")?;
        let user_id = required_str(params, "user_id")?;
        let cursor = optional_str(params, "cursor")?;
        let strategy: CrawlStrategy = match params.get("strategy") {
            None | Some(Value::Null) => CrawlStrategy {
                since: None,
                until: None,
                media_types: None,
                wanted_keys: None,
                limits: CrawlLimits::default(),
            },
            Some(value) => serde_json::from_value(value.clone())
                .map_err(|e| XError::invalid_request(format!("strategy 形状不对：{e}")))?,
        };

        // 把取数组件的端点接到爬取循环上（**进程内粘合**，docs/01 §8）
        let fetch = self.fetch.clone();
        let page_size = strategy.limits.page_size;
        let cancel_for_source = cancel.clone();
        // 顺手把**每页的完整推文**留下来。
        //
        // 为什么需要：候选（`Candidate`）是**有损**的——只有 url / ext / 几个标量，
        // 而外壳要按用户的文件名模板给文件命名、还要把任务写进自己的历史记录，
        // 那些都要推文的正文、作者昵称/id、标签、媒体宽高与"这一页里排第几"。
        // 少了它，`crawl.run` 对任何"要命名/要记账"的外壳都是不可用的，
        // 而这类外壳恰恰是主流（第一个真实消费方 CLI 不需要名字，所以没暴露这个问题）。
        // 爬取组件本身**刻意不认识 `Post`**（两个组件不互相依赖），所以拼接放在这一层。
        let collected: Arc<std::sync::Mutex<Vec<xspider_fetch::Post>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let collected_for_source = collected.clone();
        let source_fn: PageSource = Arc::new(move |page_cursor: Option<String>| {
            let fetch = fetch.clone();
            let cancel = cancel_for_source.clone();
            let user_id = user_id.clone();
            let source = source.clone();
            let cursor = page_cursor.or_else(|| cursor.clone());
            let collected = collected_for_source.clone();
            Box::pin(async move {
                let page = match source.as_str() {
                    "medias" => {
                        fetch
                            .user_medias(&user_id, cursor.as_deref(), Some(page_size), &cancel)
                            .await?
                    }
                    "tweets" => {
                        fetch
                            .user_tweets(
                                &user_id,
                                cursor.as_deref(),
                                Some(page_size),
                                Some(true),  // 只要带媒体的（候选清单就是媒体）
                                Some(false), // 丢转推
                                &cancel,
                            )
                            .await?
                    }
                    other => {
                        return Err(XError::invalid_request(format!(
                            "source 必须是 \"medias\" 或 \"tweets\"，收到 {other:?}"
                        )))
                    }
                };
                if let Ok(mut keep) = collected.lock() {
                    keep.extend(page.items.iter().cloned());
                }
                Ok(Page::new(
                    page.items.into_iter().map(to_crawl_post).collect(),
                    page.cursor,
                ))
            })
        });

        let log = self.crawl_log.clone();
        let outcome = xspider_download::crawl(&strategy, source_fn, cancel, move |event| {
            if let Ok(mut log) = log.lock() {
                log.push(event);
            }
        })
        .await?;

        // `posts` = 这一轮**保留了**的推文（即产出了候选的那些），按服务端顺序、按 id 去重。
        // 与 `candidates` 是同一批数据的两个视角：
        //   `candidates` 给"只要 URL"的消费方；`posts` 给"要命名 / 要记账"的外壳。
        let fetched = collected
            .lock()
            .map(|mut keep| std::mem::take(&mut *keep))
            .unwrap_or_default();
        let with_candidates: std::collections::HashSet<&str> = outcome
            .candidates
            .iter()
            .map(|c| c.post_id.as_str())
            .collect();
        let mut seen = std::collections::HashSet::new();
        let posts: Vec<xspider_fetch::Post> = fetched
            .into_iter()
            .filter(|post| {
                with_candidates.contains(post.id.as_str()) && seen.insert(post.id.clone())
            })
            .collect();

        let (seq, events) = self
            .crawl_log
            .lock()
            .map(|log| log.since(0))
            .unwrap_or((0, Vec::new()));
        Ok(serde_json::json!({
            "done_reason": outcome.done_reason,
            "candidates": outcome.candidates,
            "posts": posts,
            "pages": outcome.pages,
            "raw_items": outcome.raw_items,
            "dropped": outcome.dropped,
            "next_cursor": outcome.next_cursor,
            "seq": seq,
            "events": events,
        }))
    }

    fn set_cookie(&self, params: &serde_json::Map<String, Value>) -> XResult<Value> {
        let cookie = required_str(params, "cookie")?;
        let csrf = optional_str(params, "csrf")?;
        let creds = Credentials::from_cookie(cookie, csrf)?;
        self.stack.set_credentials(Some(creds));
        // 换账号之后"我是谁"就变了：不清缓存的话，关注态查询会拿旧账号的身份去问
        self.fetch.invalidate_account_cache();
        // 只回 ok：凭据**只进不出**，不回显、不确认内容
        Ok(serde_json::json!({ "ok": true }))
    }

    fn set_limits(&self, params: &serde_json::Map<String, Value>) -> XResult<Value> {
        let limits = Limits {
            api_rps: required_f64(params, "api_rps")?,
            api_burst: required_u32(params, "api_burst")?,
            cdn_concurrency: required_u32(params, "cdn_concurrency")?,
            cooldown_s: required_u64(params, "cooldown_s")?,
        };
        self.stack.set_limits(limits);
        Ok(serde_json::json!({ "ok": true }))
    }

    fn set_proxy(&self, params: &serde_json::Map<String, Value>) -> XResult<Value> {
        if !params.contains_key("url") {
            return Err(XError::invalid_request(
                "net.set_proxy 需要 url 字段（字符串指定代理，null 表示关闭）",
            ));
        }
        let proxy = ProxyConfig::from_contract(params.get("url"))?;
        self.stack.set_proxy(proxy.clone())?;
        // 下载队列**如果已经起来**，也要跟着换代理。
        // 只换取数侧会得到一个很具体的坏症状：列表刷得出来，文件一个都下不动
        // （`AGENTS.md` 踩坑记录 8：代理端口一天内变好几次）。
        if let Some(queue) = self.download.get() {
            queue.set_proxy(proxy)?;
        }
        Ok(serde_json::json!({ "ok": true }))
    }
}

/// 契约里给的是 `dest_dir` + `file_name`（目录与文件名由外壳算好，`docs/01` §4）。
fn dest_path_from(params: &serde_json::Map<String, Value>) -> XResult<std::path::PathBuf> {
    let dir = required_str(params, "dest_dir")?;
    let name = required_str(params, "file_name")?;
    // 文件名里不许出现路径分隔符：外壳给的应当是**文件名**，
    // 不是一段路径——否则 `dest_dir` 就成了摆设，也不利于跨端（Windows 的 \）。
    if name.contains('/') || name.contains('\\') {
        return Err(XError::invalid_request(
            "file_name 不能包含路径分隔符（目录请放在 dest_dir 里）",
        ));
    }
    Ok(std::path::Path::new(&dir).join(name))
}

/// 取数组件的 `Post` → 爬取循环要的最小形状。
///
/// 两个组件**不直接依赖**（`docs/01` §1），所以映射在上下层（这里）做。
fn to_crawl_post(post: xspider_fetch::Post) -> xspider_download::CrawlPost {
    xspider_download::CrawlPost {
        id: post.id,
        created_at: post.created_at,
        screen_name: Some(post.author.screen_name),
        medias: post
            .medias
            .into_iter()
            .map(|media| xspider_download::CrawlMedia {
                id: media.id,
                kind: match media.kind {
                    xspider_fetch::MediaKind::Photo => xspider_download::MediaKind::Photo,
                    xspider_fetch::MediaKind::Video => xspider_download::MediaKind::Video,
                    xspider_fetch::MediaKind::AnimatedGif => {
                        xspider_download::MediaKind::AnimatedGif
                    }
                },
                url: media.url,
                ext: media.ext,
                size_hint: None,
            })
            .collect(),
    }
}

/// 爬取事件的环形缓冲（与队列的事件日志同构：三种传输都支持"用游标轮询"）。
#[derive(Default)]
struct CrawlEventLog {
    seq: u64,
    events: Vec<(u64, xspider_download::CrawlEvent)>,
}

impl CrawlEventLog {
    const CAPACITY: usize = 2_000;

    fn push(&mut self, event: xspider_download::CrawlEvent) {
        self.seq += 1;
        if self.events.len() >= Self::CAPACITY {
            self.events.remove(0);
        }
        self.events.push((self.seq, event));
    }

    fn since(&self, from: u64) -> (u64, Vec<serde_json::Value>) {
        let items = self
            .events
            .iter()
            .filter(|(seq, _)| *seq > from)
            .map(|(seq, event)| serde_json::json!({ "seq": seq, "event": event }))
            .collect();
        (self.seq, items)
    }
}

fn ensure_object(params: &Value) -> XResult<&serde_json::Map<String, Value>> {
    match params {
        Value::Object(map) => Ok(map),
        Value::Null => Err(XError::invalid_request("params 不能是 null，应为对象 {}")),
        other => Err(XError::invalid_request(format!(
            "params 必须是对象，实际是 {}",
            type_name(other)
        ))),
    }
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn required_str(params: &serde_json::Map<String, Value>, key: &str) -> XResult<String> {
    match params.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(s.clone()),
        Some(Value::String(_)) => Err(XError::invalid_request(format!("{key} 不能是空字符串"))),
        Some(other) => Err(XError::invalid_request(format!(
            "{key} 必须是字符串，实际是 {}",
            type_name(other)
        ))),
        None => Err(XError::invalid_request(format!("缺少必填字段 {key}"))),
    }
}

fn optional_str(params: &serde_json::Map<String, Value>, key: &str) -> XResult<Option<String>> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(other) => Err(XError::invalid_request(format!(
            "{key} 必须是字符串，实际是 {}",
            type_name(other)
        ))),
    }
}

fn optional_bool(params: &serde_json::Map<String, Value>, key: &str) -> XResult<Option<bool>> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(other) => Err(XError::invalid_request(format!(
            "{key} 必须是布尔值，实际是 {}",
            type_name(other)
        ))),
    }
}

/// 可选整数，**允许 0**。
///
/// 0 是不是合法值取决于是哪个字段，不能一刀切：
/// - `dl.events.since`：契约说"从 0 开始"（0 = 从头取增量），必须是合法值；
/// - `dl.enqueue.expect_size`：schema 写的是 `minimum: 0`，空文件是合法期望；
/// - `count`（每页条数）：0 说不通——那个用 [`optional_positive_u64`]。
///
/// 这条区分是被消费者抓出来的：CLI 按 `docs/CONTRACT.md` §4.13 传了 `since: 0`，
/// 组件回 `invalid_request: since 必须是正整数`——**契约与实现不一致**，
/// 而文档是那个写对了的（`docs/06-CONSUMER-INTEGRATION.md` 记录了这次发现）。
fn optional_u64(params: &serde_json::Map<String, Value>, key: &str) -> XResult<Option<u64>> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => {
            Ok(Some(n.as_u64().ok_or_else(|| {
                XError::invalid_request(format!("{key} 必须是非负整数"))
            })?))
        }
        Some(other) => Err(XError::invalid_request(format!(
            "{key} 必须是整数，实际是 {}",
            type_name(other)
        ))),
    }
}

/// 可选整数，**必须 > 0**（用于"每页条数"这类 0 说不通的字段）。
fn optional_positive_u64(
    params: &serde_json::Map<String, Value>,
    key: &str,
) -> XResult<Option<u64>> {
    match optional_u64(params, key)? {
        Some(0) => Err(XError::invalid_request(format!("{key} 必须是正整数"))),
        other => Ok(other),
    }
}

fn required_f64(params: &serde_json::Map<String, Value>, key: &str) -> XResult<f64> {
    match params.get(key) {
        Some(Value::Number(n)) => n
            .as_f64()
            .ok_or_else(|| XError::invalid_request(format!("{key} 无法表示为浮点数"))),
        Some(other) => Err(XError::invalid_request(format!(
            "{key} 必须是数字，实际是 {}",
            type_name(other)
        ))),
        None => Err(XError::invalid_request(format!("缺少必填字段 {key}"))),
    }
}

fn required_u32(params: &serde_json::Map<String, Value>, key: &str) -> XResult<u32> {
    match params.get(key) {
        Some(Value::Number(n)) => n
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| XError::invalid_request(format!("{key} 必须是非负整数（< 2^32）"))),
        Some(other) => Err(XError::invalid_request(format!(
            "{key} 必须是整数，实际是 {}",
            type_name(other)
        ))),
        None => Err(XError::invalid_request(format!("缺少必填字段 {key}"))),
    }
}

fn required_u64(params: &serde_json::Map<String, Value>, key: &str) -> XResult<u64> {
    match params.get(key) {
        Some(Value::Number(n)) => n
            .as_u64()
            .ok_or_else(|| XError::invalid_request(format!("{key} 必须是非负整数"))),
        Some(other) => Err(XError::invalid_request(format!(
            "{key} 必须是整数，实际是 {}",
            type_name(other)
        ))),
        None => Err(XError::invalid_request(format!("缺少必填字段 {key}"))),
    }
}

/// 由环境变量构造引擎。
///
/// - `XSPIDER_FIXTURE_DIR`：**测试专用**，命中时进入 fixture 回放（不联网）。
///   放在环境变量而不是契约里：它是测试脚手架，不该让外壳看见，更不该进契约。
/// - `XSPIDER_PROXY`：显式代理，例如 `http://127.0.0.1:17890`；
///   缺省则跟随标准环境变量（`HTTPS_PROXY` / `ALL_PROXY`）。
pub fn engine_from_env() -> XResult<Engine> {
    if let Some(dir) = std::env::var_os("XSPIDER_FIXTURE_DIR") {
        return Engine::replay(dir);
    }
    let proxy = match std::env::var("XSPIDER_PROXY") {
        Ok(url) if !url.trim().is_empty() => ProxyConfig::Manual(url.trim().to_string()),
        _ => ProxyConfig::Env,
    };
    Engine::live(proxy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 一个**保证为空**的 fixture 目录。
    ///
    /// 不能直接用 `std::env::temp_dir()`：那里可能恰好躺着别的 JSON，
    /// 于是"应该缺 fixture"的用例会意外命中别人的数据（静默错误）。
    fn empty_fixture_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "xspider-empty-fixtures-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("应能创建临时目录");
        dir
    }

    fn engine() -> Engine {
        // 不需要网络的用例：直接构造一个回放到空目录的引擎
        Engine::replay(empty_fixture_dir()).expect("空 fixture 目录也能构造")
    }

    async fn call(method: &str, params: Value) -> XResult<Value> {
        engine().call(method, &params).await
    }

    #[tokio::test]
    async fn version_handshake_reports_both_versions() {
        let v = call("system.version", json!({})).await.unwrap();
        assert_eq!(v["contract_version"], CONTRACT_VERSION);
        assert_eq!(v["build_version"], BUILD_VERSION);
        // 直接构造的 Engine 是进程内形态；sidecar 会显式声明（见 xspiderd）
        assert_eq!(v["transport"], "cdylib");
        assert_eq!(engine().transport(), Transport::Cdylib);
    }

    #[tokio::test]
    async fn unknown_method_is_a_structured_invalid_request() {
        let err = call("fetch.nope", json!({})).await.unwrap_err();
        assert_eq!(err.code(), xspider_core::error::ErrorCode::InvalidRequest);
        // 错误里要列出可用 method，否则排查只能靠翻代码
        assert!(err.to_string().contains("fetch.get_user"), "{err}");
    }

    #[tokio::test]
    async fn methods_list_is_not_empty_and_contains_the_ones_we_implement() {
        let v = call("system.methods", json!({})).await.unwrap();
        let list: Vec<String> = serde_json::from_value(v["methods"].clone()).unwrap();
        for m in METHODS {
            assert!(list.contains(&(*m).to_string()), "缺 {m}");
        }
    }

    #[tokio::test]
    async fn params_must_be_an_object() {
        let err = call("fetch.get_user", json!("not an object"))
            .await
            .unwrap_err();
        assert_eq!(err.code(), xspider_core::error::ErrorCode::InvalidRequest);
        let err = call("fetch.get_user", json!(null)).await.unwrap_err();
        assert_eq!(err.code(), xspider_core::error::ErrorCode::InvalidRequest);
    }

    #[tokio::test]
    async fn missing_and_wrong_typed_fields_are_rejected_with_the_field_name() {
        let err = call("fetch.get_user", json!({})).await.unwrap_err();
        assert!(err.to_string().contains("screen_name"), "{err}");

        let err = call("fetch.get_user", json!({ "screen_name": 42 }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("screen_name"), "{err}");
        assert!(err.to_string().contains("字符串"), "{err}");

        let err = call("fetch.get_user", json!({ "screen_name": "  " }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), xspider_core::error::ErrorCode::InvalidRequest);
    }

    #[tokio::test]
    async fn set_cookie_never_echoes_the_credential() {
        let v = call(
            "auth.set_cookie",
            json!({ "cookie": "auth_token=secret-value; ct0=csrf-value" }),
        )
        .await
        .unwrap();
        assert_eq!(v, json!({ "ok": true }));
        let printed = serde_json::to_string(&v).unwrap();
        assert!(!printed.contains("secret-value"));
        assert!(!printed.contains("csrf-value"));
    }

    #[tokio::test]
    async fn set_cookie_rejects_a_cookie_without_ct0() {
        let err = call("auth.set_cookie", json!({ "cookie": "auth_token=only" }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), xspider_core::error::ErrorCode::InvalidRequest);
    }

    #[tokio::test]
    async fn set_limits_requires_every_field() {
        let ok = call(
            "net.set_limits",
            json!({ "api_rps": 1.5, "api_burst": 3, "cdn_concurrency": 2, "cooldown_s": 90 }),
        )
        .await
        .unwrap();
        assert_eq!(ok, json!({ "ok": true }));

        for missing in ["api_rps", "api_burst", "cdn_concurrency", "cooldown_s"] {
            let mut full =
                json!({ "api_rps": 1.5, "api_burst": 3, "cdn_concurrency": 2, "cooldown_s": 90 });
            full.as_object_mut().unwrap().remove(missing);
            let err = call("net.set_limits", full).await.unwrap_err();
            assert!(
                err.to_string().contains(missing),
                "缺 {missing} 时错误要点出字段名：{err}"
            );
        }
    }

    #[tokio::test]
    async fn set_proxy_accepts_url_and_null() {
        assert_eq!(
            call("net.set_proxy", json!({ "url": "http://127.0.0.1:17890" }))
                .await
                .unwrap(),
            json!({ "ok": true })
        );
        assert_eq!(
            call("net.set_proxy", json!({ "url": null })).await.unwrap(),
            json!({ "ok": true })
        );
        // 缺 url 字段 ≠ 传 null：前者是漏参数，后者是明确的"关闭代理"
        let err = call("net.set_proxy", json!({})).await.unwrap_err();
        assert_eq!(err.code(), xspider_core::error::ErrorCode::InvalidRequest);
    }

    #[tokio::test]
    async fn status_starts_ok() {
        let v = call("net.status", json!({})).await.unwrap();
        assert_eq!(v["state"], "ok");
        assert!(v.get("rate_limited_until").is_none());
    }

    /// 回放模式下 `net.probe_size` **不联网**，如实回答"服务端没说"。
    ///
    /// 这条断言盯的是铁律 3：离线测试不碰网络。就算 CDN 没被 mock，
    /// 也不能因为"用户调了这个 method"就偷偷去抓一个真实 URL。
    #[tokio::test]
    async fn probe_size_offline_says_unknown_without_touching_the_network() {
        let v = call(
            "net.probe_size",
            json!({ "url": "https://video.twimg.com/example/clip.mp4" }),
        )
        .await
        .unwrap();
        assert_eq!(v, json!({ "size": null }));
    }

    #[tokio::test]
    async fn probe_size_requires_a_url() {
        let err = call("net.probe_size", json!({})).await.unwrap_err();
        assert_eq!(err.code(), xspider_core::error::ErrorCode::InvalidRequest);
        assert!(err.to_string().contains("url"), "{err}");
    }

    /// `crawl.run` 除了候选，还要给**完整推文**。
    ///
    /// 这条钉的是一个真实的设计缺口：候选（`Candidate`）是**有损**的，只有 url 与几个标量，
    /// 而外壳要按用户的文件名模板命名、把任务写进自己的历史记录，需要正文、作者、标签、
    /// 媒体宽高与页内序号。少了 `posts`，`crawl.run` 对任何"要命名/要记账"的外壳都不可用
    /// ——第一个消费方是 CLI（它不要名字），所以这个问题直到接真实外壳才暴露。
    #[tokio::test]
    async fn crawl_returns_full_posts_alongside_candidates() {
        use xspider_core::creds::Credentials;

        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
        let stack = HttpStack::replay(&dir).expect("fixture 目录应当存在");
        // 回放的路由要求"带凭据"（fixture 是按这个条件录的），给一份假凭据即可
        stack.set_credentials(Some(
            Credentials::from_cookie("auth_token=fixture; ct0=fixture", None).unwrap(),
        ));
        let engine = Engine::with_stack(stack);

        let v = engine
            .call(
                "crawl.run",
                &json!({
                    "source": "medias",
                    "user_id": "13298072",
                    // 只跑一页：这条例外测的是"同一轮里 posts 与 candidates 对得上"，
                    // 翻页与终止判据由 xspider-download 的测试覆盖。
                    "strategy": { "limits": { "page_size": 20, "max_pages": 1 } }
                }),
            )
            .await
            .expect("回放跑一轮爬取应当成功");

        let candidates = v["candidates"].as_array().expect("有候选");
        let posts = v["posts"].as_array().expect("有完整推文");
        assert!(!candidates.is_empty(), "这页 fixture 本来就带媒体");
        assert!(!posts.is_empty(), "posts 必须一并给出");

        // 每个候选的 post_id 都要能在 posts 里找到（否则外壳拿不到"这是哪条推文"）
        let ids: std::collections::HashSet<&str> = posts
            .iter()
            .map(|p| p["id"].as_str().expect("post.id"))
            .collect();
        for c in candidates {
            let post_id = c["post_id"].as_str().expect("candidate.post_id");
            assert!(ids.contains(post_id), "候选 {post_id} 在 posts 里找不到");
        }

        // posts 里必须有候选**没有**的那些字段——这正是它存在的理由
        let first = &posts[0];
        for field in ["full_text", "author", "id"] {
            assert!(!first[field].is_null(), "post 缺少 {field}");
        }
        assert!(
            !first["full_text"].as_str().unwrap_or("").is_empty(),
            "正文不能是空的：文件名模板的 CONTENT、历史记录都要用它"
        );
        assert!(
            first["author"]["screen_name"].as_str().is_some(),
            "作者要能取到（模板的 USER_NAME / USER_ID 用它）"
        );

        // 同一批数据的两个视角：posts 里的推文必须至少产出一个候选
        for p in posts {
            let id = p["id"].as_str().unwrap();
            assert!(
                candidates.iter().any(|c| c["post_id"].as_str() == Some(id)),
                "posts 里出现了没有候选的推文：{id}"
            );
        }
    }

    #[tokio::test]
    async fn fetch_without_credentials_is_unauthorized_not_a_transport_error() {
        let err = call("fetch.get_user", json!({ "screen_name": "jack" }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), xspider_core::error::ErrorCode::Unauthorized);
        assert_eq!(err.endpoint(), Some("user_by_screen_name"));
    }

    /// 新端点：先把"参数校验"钉住（离线可测），网络行为由 replay_offline 覆盖。
    /// 0 是不是合法值**按字段区分**——这条区分是消费者抓出来的：
    /// CLI 按 `docs/CONTRACT.md` §4.13 传 `since: 0`（契约原话"从 0 开始"），
    /// 而被一刀切的正整数校验挡回来了。契约与实现不一致时，错的是实现。
    #[test]
    fn zero_is_legal_where_the_contract_says_so() {
        fn params(value: Value) -> serde_json::Map<String, Value> {
            value.as_object().expect("测试里给的是对象").clone()
        }

        // 游标轮询从 0 开始：0 必须被接受
        assert_eq!(
            optional_u64(&params(json!({ "since": 0 })), "since").unwrap(),
            Some(0)
        );
        // schema 里 expect_size 的 minimum 是 0（空文件是合法期望）
        assert_eq!(
            optional_u64(&params(json!({ "expect_size": 0 })), "expect_size").unwrap(),
            Some(0)
        );
        // 每页条数 0 说不通
        assert!(optional_positive_u64(&params(json!({ "count": 0 })), "count").is_err());
        // 负数和浮点都不是 u64
        assert!(optional_u64(&params(json!({ "since": -1 })), "since").is_err());
        assert!(optional_u64(&params(json!({ "since": 1.5 })), "since").is_err());
        // 缺省 / null 仍然是 None（"没传"与"传了 0"语义不同）
        assert_eq!(optional_u64(&params(json!({})), "since").unwrap(), None);
        assert_eq!(
            optional_u64(&params(json!({ "since": null })), "since").unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn timeline_endpoints_validate_their_parameters() {
        let e = engine();

        // 缺必填字段 → 点出字段名
        for (method, missing) in [
            ("fetch.user_medias", "user_id"),
            ("fetch.user_tweets", "user_id"),
            ("fetch.tweet_detail", "id"),
            ("fetch.search_timeline", "screen_name"),
            ("fetch.home_timeline", "mode"),
            ("fetch.following", "user_id"),
        ] {
            let err = e.call(method, &json!({})).await.unwrap_err();
            assert_eq!(
                err.code(),
                xspider_core::error::ErrorCode::InvalidRequest,
                "{method}"
            );
            assert!(err.to_string().contains(missing), "{method} → {err}");
        }

        // 类型不对也要被拒
        let err = e
            .call("fetch.user_medias", &json!({ "user_id": 42 }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("字符串"), "{err}");

        let err = e
            .call("fetch.user_tweets", &json!({ "user_id": "1", "count": 0 }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("正整数"), "{err}");

        let err = e
            .call(
                "fetch.user_tweets",
                &json!({ "user_id": "1", "include_retweets": "yes" }),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("布尔值"), "{err}");

        // 非法 mode 在**发请求之前**就被拒
        let err = e
            .call("fetch.home_timeline", &json!({ "mode": "nope" }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("for_you"), "{err}");

        // 非法日期同样在发请求之前被拒
        let err = e
            .call(
                "fetch.search_timeline",
                &json!({ "screen_name": "a", "since": "2026-02-30", "until": "2026-03-01" }),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("YYYY-MM-DD"), "{err}");
    }

    #[tokio::test]
    async fn timeline_endpoints_need_credentials_before_any_network() {
        let e = engine();
        // 未注入凭据 → unauthorized（而不是 transport 或 parse）
        for (method, params) in [
            ("fetch.user_medias", json!({ "user_id": "1" })),
            ("fetch.user_tweets", json!({ "user_id": "1" })),
            ("fetch.tweet_detail", json!({ "id": "1" })),
            ("fetch.following", json!({ "user_id": "1" })),
        ] {
            let err = e.call(method, &params).await.unwrap_err();
            assert_eq!(
                err.code(),
                xspider_core::error::ErrorCode::Unauthorized,
                "{method} → {err}"
            );
        }
    }

    #[tokio::test]
    async fn fetch_accepts_a_leading_at_sign_and_trims() {
        // "@jack" 传给 `screen_name` 时不该把 @ 送进 variables
        let stack = HttpStack::replay(empty_fixture_dir()).unwrap();
        let e = Engine::with_stack(stack);
        e.call(
            "auth.set_cookie",
            &json!({ "cookie": "auth_token=a; ct0=b" }),
        )
        .await
        .unwrap();
        let err = e
            .call("fetch.get_user", &json!({ "screen_name": "  @jack  " }))
            .await
            .unwrap_err();
        // 回放目录是空的 → 一定会缺 fixture，但报错里应该出现被 trim 过的用户名
        assert!(err.to_string().contains("jack"), "{err}");
        assert!(!err.to_string().contains("@jack"), "{err}");
    }
}
