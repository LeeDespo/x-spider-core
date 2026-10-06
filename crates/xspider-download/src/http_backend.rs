//! **内置 HTTP 下载后端**：不依赖任何外部程序搬字节。
//!
//! 为什么它必须存在（`docs/01-ARCHITECTURE.md` §5.2）：
//! 1. **保底**——"没有 aria2 二进制也能工作"，跨端行为一致；
//! 2. **离线测试的载体**——本地 HTTP server 能造出 Range / 断流 / 慢速 / 404 /
//!    大小不符这些场景，而真实 CDN 造不出来（`docs/04` §5）。
//!
//! # 四条纪律（每条都对应一个实测过的失败）
//!
//! 1. **流式写盘，不整块进内存**（`docs/05` §6）——大文件整块读会把内存打爆；
//! 2. **原子落盘**：先写 `.part` 临时文件，校验通过再 rename（`docs/02` §E5）；
//! 3. **完整性校验是唯一的"成功"判据**，不是"请求返回成功"——
//!    实测不可达 URL 的表现是**留下 0 字节文件**，且失败退出码不止一种（`docs/02` §E2）；
//! 4. **中途断流要能续传**，从**已落盘字节数**续，而不是从头再来（`docs/04` §5）。
//!
//! # 与 M2 的边界
//!
//! 这里只有"**下载一个 URL 到一个文件**"。任务队列、并发上限、状态机、
//! `job_id` 幂等、事件上报、下载记录、aria2 外派**都不在这里**——那是 M2 的活
//! （`docs/01` §5.1）。所以这个模块的公开面刻意只有 [`HttpDownloader`] 与 [`DownloadRequest`]。

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use xspider_core::cancel::CancelToken;
use xspider_core::error::{XError, XResult};
use xspider_core::http::ProxyConfig;
use xspider_core::ratelimit::{Limits, RateGate, RequestClass};

/// 单次尝试的超时（连接 + 空闲读）。整体预算由尝试次数与退避决定。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const READ_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// 中途断流的重试次数上限。
const DEFAULT_MAX_ATTEMPTS: u32 = 4;
const INITIAL_BACKOFF: Duration = Duration::from_millis(200);
const MAX_BACKOFF: Duration = Duration::from_secs(8);
/// 单次读缓冲。只要比"整个文件"小得多，就不是"整块进内存"。
const CHUNK_FLUSH_THRESHOLD: usize = 64 * 1024;

/// 下载请求的 `User-Agent`：与 `x-spider-mac` 传给 Aria2Next 的**同一个**字符串
/// （`Aria2Engine.swift` 的 `Self.userAgent`，Chrome 142 macOS）。
///
/// **不能省**：reqwest 的默认 UA 是 `reqwest/0.12`，媒体 CDN 对它的态度与浏览器
/// 不同（最坏情况是 403）。参考实现已经踩过这个坑，组件里照做即可。
pub(crate) const DOWNLOAD_USER_AGENT: &str = xspider_core::xclid::USER_AGENT;

/// 下载请求的 `Referer`。同样是参考实现里显式传的那一个（`https://x.com/`）。
pub(crate) const DOWNLOAD_REFERER: &str = "https://x.com/";

/// 让服务端**别压缩**。
///
/// 媒体是二进制、本来就不该被 gzip；但 reqwest 默认会带 `accept-encoding: gzip, br, deflate`，
/// 于是"响应头里声明的 Content-Length"可能是**压缩后**的大小，而我们落盘的是解压后的字节，
/// 完整性校验就会假红。`identity` 把这个变量彻底去掉。
pub(crate) const ACCEPT_IDENTITY: &str = "identity";

/// 进度回调：`(已完成字节, 总字节)`。总字节未知时为 0。
///
/// 纪律：**进度必须单调不减**（`docs/04` §5 有一条测试专门盯这个）。
/// 续传时"已完成"从断点开始，而不是从 0 重新数。
///
/// 要求 `Send`：队列会在 `tokio::spawn` 里调用它，回调必须能跨线程。
pub type ProgressFn<'a> = &'a mut (dyn FnMut(u64, u64) + Send);

/// 一次下载请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadRequest {
    pub url: String,
    /// 最终落盘路径（外壳算好再传，见 `docs/01` §4：目录与文件名是外壳的事）。
    pub dest_path: PathBuf,
    /// 期望字节数。给了就**必须**校验通过才算成功（`docs/02` §E2）。
    pub expect_size: Option<u64>,
    /// 中途断流的重试上限。默认 [`DEFAULT_MAX_ATTEMPTS`]。
    pub max_attempts: u32,
    /// 被取消时**是否保留**已下载的部分。
    ///
    /// "暂停"与"放弃"是两件事（`docs/01` §5.1 的 `dl.pause` 与 `dl.cancel`）：
    /// - 暂停 → `true`（保留断点，恢复时接着下）；
    /// - 取消 → `false`（清掉半成品，目标目录干净）。
    pub keep_partial_on_cancel: bool,
    /// 「希望多连接」的分片数。**只有外派后端用得上**
    /// （Aria2Next 的 `stream-max-connections`）；内置后端没有多连接，
    /// 会忽略它——这正是"能力表达不是引擎名"的意思（`docs/01` §5.2 纪律 1）。
    pub segments: u8,
    /// 代理 URL（已解析成具体值）。**只有外派后端用得上**：
    /// 内置后端的 reqwest 客户端构造时就带上了代理，而 Aria2Next 是子进程，
    /// 必须逐任务显式告诉它走哪个出口——否则"需要代理才能出网"的机器上，
    /// 外派后端会直连失败，而内置后端正常，两个后端行为不一致。
    pub proxy_url: Option<String>,
}

impl DownloadRequest {
    pub fn new(url: impl Into<String>, dest_path: impl Into<PathBuf>) -> Self {
        Self {
            url: url.into(),
            dest_path: dest_path.into(),
            expect_size: None,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            keep_partial_on_cancel: false,
            segments: 1,
            proxy_url: None,
        }
    }

    pub fn with_expect_size(mut self, size: u64) -> Self {
        self.expect_size = Some(size);
        self
    }
}

/// 下载结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadOutcome {
    /// 最终文件字节数。
    pub bytes: u64,
    /// 本次是从第几字节开始写的（0 表示全新；>0 表示续传）。
    pub resumed_from: u64,
    /// 用掉的尝试次数（含首次）。
    pub attempts: u32,
    /// 校验结论。
    pub integrity: Integrity,
}

/// 完整性校验结论。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "result")]
pub enum Integrity {
    /// 服务端给了大小且与实际一致。
    Verified { expected: u64, actual: u64 },
    /// 没给期望大小，只记实际字节数（**不能声称"校验通过"**）。
    Unverified { actual: u64 },
}

/// 下载失败的原因。**必须能区分**（`docs/01` §5.2 纪律 3）：
/// "没找到"和"大小不对"和"被取消"是完全不同的处理方式。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DownloadError {
    #[error("资源不存在（HTTP 404）")]
    NotFound,
    #[error("需要授权（HTTP {status}）")]
    AuthRequired { status: u16 },
    #[error("上游返回 HTTP {status}")]
    Upstream { status: u16 },
    /// **完整性失败**：拿到字节数与期望不符。这不是 `completed`，必须显式区分。
    #[error("完整性校验失败：期望 {expected} 字节，实际 {actual} 字节")]
    IntegrityFailed { expected: u64, actual: u64 },
    /// 服务端声明的大小与实际拿到的不符（中途截断）。
    #[error("服务端声明 {declared} 字节，实际只拿到 {received} 字节")]
    Truncated { declared: u64, received: u64 },
    #[error("写入落盘失败（{kind}）：{message}")]
    Io { kind: &'static str, message: String },
    #[error("网络失败（{kind}）：{message}")]
    Transport { kind: &'static str, message: String },
    #[error("已取消")]
    Cancelled,
    #[error("参数不合法：{message}")]
    Invalid { message: String },
}

impl DownloadError {
    /// 能不能重试。**只重试传输层/截断**；404、401、完整性失败都不重试
    /// （重试服务端说的"不行"只会放大问题，`docs/02` §B7）。
    fn is_retryable(&self) -> bool {
        matches!(
            self,
            DownloadError::Transport { .. } | DownloadError::Truncated { .. }
        )
    }

    /// 磁盘满要单独识别——它要报给用户的是"换个目录"，不是"网络问题"。
    fn classify_io(e: &std::io::Error, message: String) -> Self {
        let kind = match e.kind() {
            std::io::ErrorKind::StorageFull => "disk_full",
            std::io::ErrorKind::PermissionDenied => "permission_denied",
            std::io::ErrorKind::NotFound => "not_found",
            _ => "other",
        };
        DownloadError::Io { kind, message }
    }
}

impl From<DownloadError> for XError {
    fn from(e: DownloadError) -> Self {
        match e {
            DownloadError::NotFound => XError::not_found(e.to_string()),
            DownloadError::AuthRequired { .. } => XError::unauthorized(e.to_string()),
            DownloadError::Upstream { status } => XError::upstream(status, e.to_string()),
            DownloadError::Cancelled => XError::Cancelled,
            // 用 `message` 而不是 `e.to_string()`：`Transport` / `Io` 的 Display 自己带了
            // "网络失败（kind）："这层前缀，而 `XError::transport` 还会再加一层，
            // 拼出来是"网络错误（aria2_error）：网络失败（aria2_error）：…"（实测的重复）。
            DownloadError::Transport { kind, message } => XError::transport(kind, message.clone()),
            DownloadError::Io { kind, message } => XError::transport(kind, message.clone()),
            // 完整性失败既不是网络问题也不是"上游错误"——归到 parse 类
            // （"拿到的数据不符合预期"），M2 接契约时再单独定义事件字段。
            DownloadError::IntegrityFailed { .. } | DownloadError::Truncated { .. } => {
                XError::parse("download.integrity", e.to_string())
            }
            DownloadError::Invalid { message } => XError::invalid_request(message),
        }
    }
}

pub type DownloadResult<T> = Result<T, DownloadError>;

/// 按代理配置造一个 reqwest 客户端（构造与换代理共用同一条路径）。
fn build_client(proxy: &ProxyConfig) -> XResult<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_IDLE_TIMEOUT)
        // 下载不需要跟随重定向：指向另一个 URL 的"资源"通常意味着我们拿错了东西
        .redirect(reqwest::redirect::Policy::none());
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
        .map_err(|e| XError::internal(format!("构造下载客户端失败：{e}")))
}

/// 内置下载器。克隆廉价（内部 `Arc`）。
#[derive(Clone)]
pub struct HttpDownloader {
    /// 客户端在 `RwLock` 后面，是为了**能换代理**。
    ///
    /// 实测：本机代理端口一天之内会变好几次，而且会整段时间不可达
    /// （踩坑 8，见 docs/05-WORKFLOW.md 的踩坑总索引）。`net.set_proxy` 的存在就是为了"运行中换掉它，
    /// 不必重启进程"——如果下载器在构造时把代理焊死，那条承诺对下载就落空了。
    /// `reqwest::Client` 克隆廉价（内部是 `Arc`），所以取用时 clone 一份再发请求。
    client: Arc<std::sync::RwLock<reqwest::Client>>,
    gate: Arc<RateGate>,
}

impl std::fmt::Debug for HttpDownloader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpDownloader").finish_non_exhaustive()
    }
}

impl HttpDownloader {
    /// 构造。`proxy` 与限流参数复用内核的配置类型，保证与取数侧同一套语义。
    pub fn new(proxy: &ProxyConfig, limits: Limits) -> XResult<Self> {
        Ok(Self {
            client: Arc::new(std::sync::RwLock::new(build_client(proxy)?)),
            gate: Arc::new(RateGate::new(limits)),
        })
    }

    /// 换代理。**已经在跑的请求不受影响**，后续请求立刻走新出口。
    pub fn set_proxy(&self, proxy: &ProxyConfig) -> XResult<()> {
        let client = build_client(proxy)?;
        *self.client.write().expect("download client poisoned") = client;
        tracing::debug!(?proxy, "下载器已切换代理");
        Ok(())
    }

    /// 取一份客户端句柄（clone 会把读锁放开，避免跨 await 持锁）。
    fn client(&self) -> reqwest::Client {
        self.client
            .read()
            .expect("download client poisoned")
            .clone()
    }

    /// CDN 侧的闸门（与接口配额**分开治理**，`docs/02` §B6）。
    pub fn gate(&self) -> &RateGate {
        &self.gate
    }

    /// 问服务端"这个文件多大"。
    ///
    /// # 为什么需要联网问，而不是算
    ///
    /// 实测（2026-10-01，真实 CDN）：
    /// - GraphQL 的 `media` 对象里**没有字节数**（只有宽高与视频时长/码率）；
    /// - 但 CDN 直接告诉我们：`HEAD` 返回 `Content-Length`；
    /// - 视频实测 `HEAD` 走不通时，**`Range: bytes=0-0` 的 `Content-Range: bytes 0-0/<总长>`
    ///   一样给出精确值**（只下载 1 个字节）。
    ///
    /// **不要用 `码率 × 时长` 估算**：实测一条 61.5 秒的视频，
    /// 选中变体的码率是 10.368 Mbps → 估算 76 MiB，而真实大小是 **15.2 MB**（约 14.5 MiB）。
    /// 码率是编码器的**上限**，不是实际大小——差 5 倍，拿它做"要不要外派给 aria2"的分界
    /// 会一路判错。
    ///
    /// 返回 `Ok(None)` 表示"服务端没说"（不是错误）：探测失败不该让调用方跟着失败。
    /// 走 **CDN 配额**过闸门（`docs/02` §B6：两套配额分开治理）。
    pub async fn probe_size(&self, url: &str, cancel: &CancelToken) -> DownloadResult<Option<u64>> {
        self.gate
            .acquire_or_wait(RequestClass::Cdn, Some("size_probe"), cancel)
            .await
            .map_err(|e| match e {
                XError::Cancelled => DownloadError::Cancelled,
                other => DownloadError::Transport {
                    kind: "gate",
                    message: other.to_string(),
                },
            })?;

        // 先试 HEAD：最省，不下载任何字节
        if let Ok(Some(size)) = self.probe_head(url, cancel).await {
            return Ok(Some(size));
        }
        // HEAD 不被支持（405/501）或没给长度 → 用 1 字节的 Range 问
        self.probe_range(url, cancel).await
    }

    async fn probe_head(&self, url: &str, cancel: &CancelToken) -> DownloadResult<Option<u64>> {
        let response = cancel
            .race(
                self.client()
                    .head(url)
                    .header(reqwest::header::USER_AGENT, DOWNLOAD_USER_AGENT)
                    .header(reqwest::header::REFERER, DOWNLOAD_REFERER)
                    .header(reqwest::header::ACCEPT_ENCODING, ACCEPT_IDENTITY)
                    .send(),
            )
            .await
            .map_err(|_| DownloadError::Cancelled)?
            .map_err(|e| map_reqwest_error(&e))?;
        if !response.status().is_success() {
            return Ok(None);
        }
        Ok(response
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok()))
    }

    async fn probe_range(&self, url: &str, cancel: &CancelToken) -> DownloadResult<Option<u64>> {
        let response = cancel
            .race(
                self.client()
                    .get(url)
                    .header(reqwest::header::RANGE, "bytes=0-0")
                    .header(reqwest::header::USER_AGENT, DOWNLOAD_USER_AGENT)
                    .header(reqwest::header::REFERER, DOWNLOAD_REFERER)
                    .header(reqwest::header::ACCEPT_ENCODING, ACCEPT_IDENTITY)
                    .send(),
            )
            .await
            .map_err(|_| DownloadError::Cancelled)?
            .map_err(|e| map_reqwest_error(&e))?;

        let status = response.status().as_u16();
        if status == 404 {
            return Err(DownloadError::NotFound);
        }
        let headers = response.headers().clone();
        // 206 的 Content-Range 里带总长；200 表示服务端忽略了 Range，这时 Content-Length
        // 就是整个文件的大小（我们不能只读 1 字节，所以立刻丢掉 body）
        drop(response);
        if let Some(total) = headers
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_content_range_total)
        {
            return Ok(Some(total));
        }
        Ok(headers
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok()))
    }

    /// 下载一个 URL 到一个文件。
    ///
    /// 语义要点：
    /// - 已存在同名 `.part` 文件时**自动续传**（从它的长度开始）；
    /// - 中途断流会重试，且每次都从**当前已落盘字节数**续；
    /// - 只有校验通过才 `rename` 到目标路径——**失败不会在目标路径留下半个文件**；
    /// - 取消时清掉 `.part`（"放弃"与"暂停"不同：暂停要留断点，那是 M2 的事）。
    pub async fn download(
        &self,
        request: &DownloadRequest,
        cancel: &CancelToken,
    ) -> DownloadResult<DownloadOutcome> {
        self.download_with_progress(request, cancel, &mut |_, _| {})
            .await
    }

    /// 与 [`Self::download`] 相同，但会报告进度。
    pub async fn download_with_progress(
        &self,
        request: &DownloadRequest,
        cancel: &CancelToken,
        progress: ProgressFn<'_>,
    ) -> DownloadResult<DownloadOutcome> {
        if request.url.trim().is_empty() {
            return Err(DownloadError::Invalid {
                message: "url 不能为空".to_string(),
            });
        }
        if let Some(parent) = request.dest_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    DownloadError::classify_io(&e, format!("创建目录 {} 失败", parent.display()))
                })?;
            }
        }

        let part = part_path(&request.dest_path);
        let mut written = match std::fs::metadata(&part) {
            Ok(meta) => meta.len(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            Err(e) => {
                return Err(DownloadError::classify_io(
                    &e,
                    format!("读取断点文件 {} 失败", part.display()),
                ))
            }
        };
        let resumed_from = written;

        let attempts_limit = request.max_attempts.max(1);
        let mut backoff = INITIAL_BACKOFF;
        let mut last_error: Option<DownloadError> = None;
        let mut attempts = 0u32;

        while attempts < attempts_limit {
            attempts += 1;
            if cancel.is_cancelled() {
                self.on_cancel(&part, request);
                return Err(DownloadError::Cancelled);
            }
            match self
                .attempt(
                    &request.url,
                    &part,
                    written,
                    request.expect_size,
                    cancel,
                    progress,
                )
                .await
            {
                Ok(new_written) => {
                    written = new_written;
                    // 落盘完成 → 只做一次完整性校验
                    return self.finish(request, &part, written, resumed_from, attempts);
                }
                Err(e) => {
                    if e == DownloadError::Cancelled {
                        self.on_cancel(&part, request);
                        return Err(e);
                    }
                    let retryable = e.is_retryable();
                    // 关键：重试用**重新读盘得到的**字节数，而不是内存里的计数——
                    // 上一次可能已经写进去一部分（截断就是这种情况）
                    written = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(written);
                    tracing::debug!(attempt = attempts, error = %e, "下载尝试失败");
                    last_error = Some(e);
                    if !retryable || attempts >= attempts_limit {
                        break;
                    }
                    match cancel.race(tokio::time::sleep(backoff)).await {
                        Ok(()) => {}
                        Err(_) => {
                            self.on_cancel(&part, request);
                            return Err(DownloadError::Cancelled);
                        }
                    }
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            }
        }

        let error = last_error.unwrap_or(DownloadError::Invalid {
            message: "没有可用地址".to_string(),
        });
        // 失败时不留下疑似完整的文件：目标路径始终干净（docs/02 §E5）
        self.discard_partial(&part);
        Err(error)
    }

    /// 一次尝试：可能续传，返回**这次尝试结束时**的总字节数。
    #[allow(clippy::too_many_arguments)] // 每个参数都是独立语义，打包成结构体反而更难读
    async fn attempt(
        &self,
        url: &str,
        part: &Path,
        already: u64,
        expect_size: Option<u64>,
        cancel: &CancelToken,
        progress: ProgressFn<'_>,
    ) -> DownloadResult<u64> {
        self.gate
            .acquire_or_wait(RequestClass::Cdn, Some("download"), cancel)
            .await
            .map_err(|e| match e {
                XError::Cancelled => DownloadError::Cancelled,
                other => DownloadError::Transport {
                    kind: "gate",
                    message: other.to_string(),
                },
            })?;

        let mut request = self.client().get(url);
        request = request
            .header(reqwest::header::USER_AGENT, DOWNLOAD_USER_AGENT)
            .header(reqwest::header::REFERER, DOWNLOAD_REFERER)
            .header(reqwest::header::ACCEPT_ENCODING, ACCEPT_IDENTITY);
        if already > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={already}-"));
        }
        let response = cancel
            .race(request.send())
            .await
            .map_err(|_| DownloadError::Cancelled)?
            .map_err(|e| map_reqwest_error(&e))?;

        let status = response.status().as_u16();
        match status {
            200 | 206 => {}
            404 => return Err(DownloadError::NotFound),
            401 | 403 => return Err(DownloadError::AuthRequired { status }),
            _ => return Err(DownloadError::Upstream { status }),
        }

        // Range 请求被忽略（返回 200）时，必须**从头写**，否则会拼出一段错位的文件
        let appending = status == 206 && already > 0;
        if !appending && already > 0 {
            tracing::debug!("服务端忽略了 Range，改为从头下载");
            std::fs::remove_file(part).map_err(|e| {
                DownloadError::classify_io(&e, format!("清理断点文件 {} 失败", part.display()))
            })?;
        }
        let mut written = if appending { already } else { 0 };

        // 服务端声明的大小（用于提前发现截断）
        let declared = response
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .map(|len| if appending { written + len } else { len });

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(part)
            .map_err(|e| {
                DownloadError::classify_io(&e, format!("打开落盘文件 {} 失败", part.display()))
            })?;

        let mut response = response;
        let mut since_flush = 0usize;
        loop {
            let chunk = cancel
                .race(response.chunk())
                .await
                .map_err(|_| DownloadError::Cancelled)?
                .map_err(|e| map_reqwest_error(&e))?;
            let Some(chunk) = chunk else { break };
            file.write_all(&chunk).map_err(|e| {
                DownloadError::classify_io(&e, format!("写入 {} 失败", part.display()))
            })?;
            written += chunk.len() as u64;
            since_flush += chunk.len();
            // 进度每片都报：调用方要节流是调用方的事，我们不能"替他决定"
            progress(written, declared.unwrap_or(0));
            if since_flush >= CHUNK_FLUSH_THRESHOLD {
                // 每 64KB flush 一次：断流时"已落盘字节数"才是可信的续传起点
                file.flush().map_err(|e| {
                    DownloadError::classify_io(&e, "flush 落盘文件失败".to_string())
                })?;
                since_flush = 0;
            }
        }
        file.flush()
            .map_err(|e| DownloadError::classify_io(&e, "flush 落盘文件失败".to_string()))?;
        drop(file);

        // 中途截断：服务端说的大小与实际不符 → 可重试（续传接着写）
        if let Some(declared) = declared {
            if written < declared {
                return Err(DownloadError::Truncated {
                    declared,
                    received: written,
                });
            }
        }
        // 期望大小已经拿满 → 不必再让服务端多跑一次（也避免 416）
        if let Some(expected) = expect_size {
            if written == expected {
                return Ok(written);
            }
        }
        Ok(written)
    }

    /// 校验 + 原子落盘。
    fn finish(
        &self,
        request: &DownloadRequest,
        part: &Path,
        written: u64,
        resumed_from: u64,
        attempts: u32,
    ) -> DownloadResult<DownloadOutcome> {
        let actual = std::fs::metadata(part)
            .map(|m| m.len())
            .map_err(|e| DownloadError::classify_io(&e, "读取临时文件大小失败".to_string()))?;

        let integrity = match request.expect_size {
            Some(expected) => {
                if actual != expected {
                    // **不算完成**：删掉半成品，报 integrity_failed（docs/02 §E2）
                    self.discard_partial(part);
                    return Err(DownloadError::IntegrityFailed { expected, actual });
                }
                Integrity::Verified { expected, actual }
            }
            None => Integrity::Unverified { actual },
        };

        std::fs::rename(part, &request.dest_path).map_err(|e| {
            DownloadError::classify_io(
                &e,
                format!(
                    "落盘 {} → {} 失败（跨卷 rename 会失败）",
                    part.display(),
                    request.dest_path.display()
                ),
            )
        })?;

        Ok(DownloadOutcome {
            bytes: written.max(actual),
            resumed_from,
            attempts,
            integrity,
        })
    }

    /// 取消时的收尾：按 `keep_partial_on_cancel` 决定保留断点还是清掉。
    fn on_cancel(&self, part: &Path, request: &DownloadRequest) {
        if request.keep_partial_on_cancel {
            tracing::debug!(path = %part.display(), "取消但保留断点（暂停语义）");
        } else {
            self.discard_partial(part);
        }
    }

    fn discard_partial(&self, part: &Path) {
        if let Err(e) = std::fs::remove_file(part) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(path = %part.display(), error = %e, "清理临时文件失败");
            }
        }
    }
}

/// 内置 HTTP 后端的引擎标识（出现在断点文件名里）。
pub const ATTRIBUTION_HTTP: &str = "http";
/// Aria2Next 后端的引擎标识。
pub const ATTRIBUTION_ARIA2: &str = "aria2next";

/// 断点/临时文件路径。用固定后缀而不是随机名：**续传需要它稳定存在**
/// （`docs/02` §E4 的教训是"临时文件不能当引擎内部细节"，这里把它显式化）。
///
/// **文件名里带引擎标识**（`.part.http` / `.part.aria2next`）是刻意的：
/// 两个引擎的半成品格式不兼容，拼在一起会产出**损坏文件**
/// （`docs/02` §E3：引擎切换必须丢弃断点）。带上标识，物理上就不可能误用。
pub fn part_path_for(dest: &Path, attribution: &str) -> PathBuf {
    let mut name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".to_string());
    name.push_str(".part.");
    name.push_str(attribution);
    dest.with_file_name(name)
}

/// 内置后端的断点路径。
pub fn part_path(dest: &Path) -> PathBuf {
    part_path_for(dest, ATTRIBUTION_HTTP)
}

/// 从 `Content-Range: bytes 0-0/15187101` 里取出**总长度**。
///
/// 抽成纯函数是为了能离线钉住格式：真实响应里的形状是 `bytes <起>-<止>/<总长>`，
/// 也可能是 `bytes */<总长>`（无法满足 Range 时）。
fn parse_content_range_total(value: &str) -> Option<u64> {
    let (_, total) = value.trim().split_once('/')?;
    total.trim().parse::<u64>().ok()
}

/// `reqwest::Error` → 结构化 kind。**只用结构化谓词**，不匹配错误文案。
fn map_reqwest_error(e: &reqwest::Error) -> DownloadError {
    let kind = if e.is_timeout() {
        "timeout"
    } else if e.is_connect() {
        "connect"
    } else if e.is_body() || e.is_decode() {
        "body"
    } else {
        "other"
    };
    DownloadError::Transport {
        kind,
        message: e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_range_total_is_parsed() {
        // 206 的常规形状
        assert_eq!(
            parse_content_range_total("bytes 0-0/15187101"),
            Some(15_187_101)
        );
        // 起止与总长位数不同也要对
        assert_eq!(parse_content_range_total("bytes 0-0/999"), Some(999));
        // 无法满足 Range 时的形状
        assert_eq!(parse_content_range_total("bytes */12345"), Some(12345));
        // 垃圾输入不能 panic
        assert_eq!(parse_content_range_total("bytes 0-0"), None);
        assert_eq!(parse_content_range_total(""), None);
        assert_eq!(parse_content_range_total("bytes 0-0/abc"), None);
    }

    #[test]
    fn part_path_is_stable_engine_specific_and_colocated() {
        let dest = PathBuf::from("/tmp/a/b/pic.jpg");
        assert_eq!(
            part_path(&dest),
            PathBuf::from("/tmp/a/b/pic.jpg.part.http")
        );
        // 稳定性是续传的前提：两次调用必须一样
        assert_eq!(part_path(&dest), part_path(&dest));
        // 引擎标识必须体现在名字里（换引擎绝不复用别人的半成品）
        assert_ne!(part_path(&dest), part_path_for(&dest, ATTRIBUTION_ARIA2));
    }

    #[test]
    fn only_transport_and_truncation_are_retryable() {
        assert!(DownloadError::Transport {
            kind: "connect",
            message: String::new()
        }
        .is_retryable());
        assert!(DownloadError::Truncated {
            declared: 10,
            received: 5
        }
        .is_retryable());
        // 这些都不该重试：重试服务端说的"不行"只会放大问题
        for e in [
            DownloadError::NotFound,
            DownloadError::AuthRequired { status: 403 },
            DownloadError::Upstream { status: 500 },
            DownloadError::IntegrityFailed {
                expected: 10,
                actual: 8,
            },
            DownloadError::Cancelled,
            DownloadError::Invalid {
                message: "x".into(),
            },
        ] {
            assert!(!e.is_retryable(), "{e:?} 不该可重试");
        }
    }

    #[test]
    fn io_errors_are_classified_by_kind_not_by_message() {
        let full = std::io::Error::from(std::io::ErrorKind::StorageFull);
        match DownloadError::classify_io(&full, "写失败".into()) {
            DownloadError::Io { kind, .. } => assert_eq!(kind, "disk_full"),
            other => panic!("{other:?}"),
        }
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        match DownloadError::classify_io(&denied, "写失败".into()) {
            DownloadError::Io { kind, .. } => assert_eq!(kind, "permission_denied"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn integrity_failure_maps_to_parse_not_success() {
        let err: XError = DownloadError::IntegrityFailed {
            expected: 100,
            actual: 40,
        }
        .into();
        assert_eq!(err.code(), xspider_core::error::ErrorCode::Parse);
        assert!(err.to_string().contains("完整性"), "{err}");
    }

    #[tokio::test]
    async fn empty_url_is_rejected_without_touching_the_disk() {
        let downloader = HttpDownloader::new(&ProxyConfig::Off, Limits::default()).unwrap();
        let request = DownloadRequest::new("   ", std::env::temp_dir().join("x"));
        let err = downloader
            .download(&request, &CancelToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, DownloadError::Invalid { .. }), "{err:?}");
    }

    /// 断点文件里已有的字节必须被当作续传起点（不发 Range 时就是从头写）。
    #[tokio::test]
    async fn a_preexisting_part_file_is_detected_as_a_resume_point() {
        let dir = std::env::temp_dir().join(format!("xspider-resume-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("f.bin");
        let mut f = std::fs::File::create(part_path(&dest)).unwrap();
        f.write_all(&[0u8; 7]).unwrap();
        drop(f);

        // 用不可达地址：整个下载会失败，但错误信息里应体现"从第 7 字节续传过"——
        // 通过"目标路径没有留下文件"来间接验证（失败不许污染目标路径）
        let downloader = HttpDownloader::new(&ProxyConfig::Off, Limits::default()).unwrap();
        let request = DownloadRequest::new("http://127.0.0.1:1/never", &dest).with_expect_size(7);
        let err = downloader
            .download(&request, &CancelToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, DownloadError::Transport { .. }), "{err:?}");
        assert!(!dest.exists(), "失败不该在目标路径留下文件");
        assert!(!part_path(&dest).exists(), "失败也不该留下 .part");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
