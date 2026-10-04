//! **下载队列与任务状态机**（`docs/01-ARCHITECTURE.md` §5.1）。
//!
//! 这一层回答四个问题，每个都有明确的唯一所有者：
//!
//! | 问题 | 归属 | 为什么 |
//! |---|---|---|
//! | 并发上限 | **这里** | 并发必须有唯一所有者，否则 CDN 限流降级失效 |
//! | 任务状态机 | **这里** | 跨端一致 |
//! | `job_id` 幂等 | **这里** | 一次解决「跨页重复投递 / 暂停后重试 / 重启对账」三件事 |
//! | 下载记录 | **这里** | 两个写者必然出现"文件下好了但记录没写"的静默不一致 |
//!
//! # 与后端的边界（这一条是刻意的）
//!
//! 后端（内置 HTTP / Aria2Next）只负责"把字节搬过来"。**生命周期策略在队列这边**：
//! 队列总是让后端 `keep_partial_on_cancel`——后端不删半成品，
//! 由队列决定"这次取消是暂停（留断点）还是放弃（清干净）"。
//! 否则每个后端都要自己实现一遍暂停语义，迟早分叉。
//!
//! # 引擎选择留在组件内部
//!
//! `docs/01` §5.2 纪律 1：**引擎选择策略不暴露给外壳**。这里是唯一的实现点——
//! 外壳只说"要不要断点续传、要几路分片"（`requirements`），
//! 由组件按大小与可用性决定派给谁。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, OwnedSemaphorePermit, Semaphore};

use xspider_core::cancel::CancelToken;
use xspider_core::error::{ErrorObject, XError, XResult};
use xspider_core::http::ProxyConfig;
use xspider_core::ratelimit::Limits;

use crate::aria2::{Aria2Next, Aria2NextConfig};
use crate::http_backend::{
    part_path_for, DownloadError, DownloadOutcome, DownloadRequest, HttpDownloader, Integrity,
    ATTRIBUTION_ARIA2, ATTRIBUTION_HTTP,
};
use crate::{JobId, JobState};

/// 任务要求。**外壳用能力表达意图**，而不是点某个引擎的名字（`docs/01` §5.2 纪律 2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requirements {
    /// 需要断点续传（暂停/失败后接着下）。
    #[serde(default = "default_true")]
    pub resume: bool,
    /// 期望的分片数。`> 1` 表示希望多连接，内置后端做不到，会派给 Aria2Next。
    #[serde(default = "default_segments")]
    pub segments: u8,
}

fn default_true() -> bool {
    true
}
fn default_segments() -> u8 {
    1
}

impl Default for Requirements {
    fn default() -> Self {
        Self {
            resume: true,
            segments: 1,
        }
    }
}

/// 入队请求。
#[derive(Debug, Clone)]
pub struct EnqueueJob {
    /// **外壳生成**的幂等键（`docs/01` §5.1）。
    pub job_id: JobId,
    pub url: String,
    /// 最终落盘路径。目录与文件名由外壳算好再传（`docs/01` §4）。
    pub dest_path: PathBuf,
    pub expect_size: Option<u64>,
    pub requirements: Requirements,
    /// 不透明标记：组件**只存不解释**（它不可能知道 TwitterPost 是什么类型）。
    pub tag: Option<String>,
    /// 已经下好且校验一致就跳过（`docs/02` §E1：这是**用户设置项**，不是实现细节）。
    pub skip_if_present: bool,
}

impl EnqueueJob {
    pub fn new(
        job_id: impl Into<String>,
        url: impl Into<String>,
        dest_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            job_id: JobId::new(job_id),
            url: url.into(),
            dest_path: dest_path.into(),
            expect_size: None,
            requirements: Requirements::default(),
            tag: None,
            skip_if_present: false,
        }
    }

    /// 带上期望大小（**强烈建议给**：没有它就只能是"未校验"，`docs/02` §E2）。
    pub fn with_expect_size(mut self, size: u64) -> Self {
        self.expect_size = Some(size);
        self
    }
}

/// 入队结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceptedBy {
    /// 新任务，已排队。
    Queued,
    /// **同一个 `job_id` 已经存在**——只算一次（幂等）。
    AlreadyKnown,
    /// 已经下好且一致，跳过。
    Skipped,
}

/// 事件。外壳靠它驱动进度条与通知（组件只发事件，不做通知）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DownloadEvent {
    /// 进度。**单调不减**（续传时 `done` 从断点开始）。
    Progress {
        job_id: String,
        done: u64,
        total: u64,
    },
    Completed {
        job_id: String,
        path: String,
        bytes: u64,
        integrity: Integrity,
    },
    /// 失败。`reason` 是**结构化的短标签**（不匹配文案）。
    Failed {
        job_id: String,
        reason: String,
        error: Box<ErrorObject>,
    },
    Skipped {
        job_id: String,
        reason: String,
    },
}

impl DownloadEvent {
    pub fn job_id(&self) -> &str {
        match self {
            DownloadEvent::Progress { job_id, .. }
            | DownloadEvent::Completed { job_id, .. }
            | DownloadEvent::Failed { job_id, .. }
            | DownloadEvent::Skipped { job_id, .. } => job_id,
        }
    }
}

/// 任务快照（`dl.status` / `dl.list` 的形状）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobSnapshot {
    pub job_id: String,
    pub state: JobState,
    pub done: u64,
    pub total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorObject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dest_path: Option<String>,
}

/// 队列配置。
#[derive(Debug, Clone)]
pub struct QueueConfig {
    pub limits: Limits,
    /// 代理。文件下载走 CDN，与接口共用同一套代理配置。
    pub proxy: ProxyConfig,
    /// 配置了就用 Aria2Next；没配就只用内置后端。
    pub aria2: Option<Aria2NextConfig>,
    /// 下载记录文件路径。给了才会持久化与做重启对账。
    pub records_path: Option<PathBuf>,
    /// 小于这个大小就走内置后端（省掉一个子进程的启动开销）。
    pub min_size_for_aria2: u64,
    /// 调用方没给 `expect_size` 时，**问服务端要**（`HEAD`，失败退回 1 字节 Range）。
    ///
    /// 默认开。理由：GraphQL 的 media 对象里没有字节数，而"下完之后不知道该有多大"
    /// 就意味着**没法校验完整性**（`docs/02` §E2）。一次 HEAD 的代价远小于
    /// "下到一个残缺文件还以为成功了"。
    ///
    /// **代价要认**：一次探测 = 一次 CDN 请求。参考实现（`x-spider-mac` 的
    /// `DOWNLOAD_ENGINE_PLAN.md`）因此**刻意不探**，改用 `码率 × 时长` 估算——
    /// 而实测那个估算差 5.25 倍（`docs/02` §E9），只能用于"派给哪个引擎"这类
    /// 粗粒度判断。所以这里保留默认开，但给了关掉的开关
    /// （引擎侧 `XSPIDER_PROBE_SIZE=0`）：配额敏感的场景由外壳自己决定。
    pub probe_size_when_unknown: bool,
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            proxy: ProxyConfig::Env,
            aria2: None,
            records_path: None,
            // 5 MiB：与参考实现的默认阈值一致（`Settings.swift` 的
            // `aria2SizeThresholdMB = 5`）。比这更小的文件，aria2 的多连接
            // 还没预热完就下完了。
            min_size_for_aria2: 5 * 1024 * 1024,
            probe_size_when_unknown: true,
        }
    }
}

/// 一次实际派发的后端（**测试与日志用**；不进契约）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineChoice {
    Http,
    Aria2Next,
}

/// 这次"停"是要暂停还是放弃。**两者对半成品的处理完全相反**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CancelIntent {
    /// 没有停过（正常完成/失败）。
    None,
    /// 暂停：**保留断点**，恢复时接着下。
    Pause,
    /// 放弃：清掉半成品，目标目录干净。
    Discard,
}

#[derive(Debug)]
struct JobEntry {
    job: EnqueueJob,
    state: JobState,
    done: u64,
    total: u64,
    reason: Option<String>,
    error: Option<ErrorObject>,
    cancel: CancelToken,
    intent: CancelIntent,
    engine: Option<EngineChoice>,
    attempts: u32,
    /// 派发序号。**每次派发都 +1**，任务只在自己那一轮里落状态。
    ///
    /// 为什么需要它：暂停→恢复会立刻派发新一轮，而上一轮的任务可能还在收尾；
    /// 没有 epoch 的话，上一轮的"已取消"会把新一轮刚设好的 waiting 覆盖成 error
    /// （实现时真的踩到了，见 AGENTS.md 踩坑记录）。
    epoch: u64,
}

impl JobEntry {
    fn snapshot(&self) -> JobSnapshot {
        JobSnapshot {
            job_id: self.job.job_id.to_string(),
            state: self.state,
            done: self.done,
            total: self.total,
            reason: self.reason.clone(),
            error: self.error.clone(),
            tag: self.job.tag.clone(),
            dest_path: Some(self.job.dest_path.display().to_string()),
        }
    }
}

/// 带事件序号的事件，用于"用游标轮询"（见 [`DownloadQueue::events_since`]）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SequencedEvent {
    pub seq: u64,
    #[serde(flatten)]
    pub event: DownloadEvent,
}

struct Inner {
    jobs: Mutex<HashMap<String, JobEntry>>,
    events: broadcast::Sender<DownloadEvent>,
    /// 事件环形缓冲：给"用游标轮询"用（三种传输都支持请求/响应，不支持流）。
    log: Mutex<(u64, Vec<SequencedEvent>)>,
    records: Mutex<HashMap<String, RecordEntry>>,
    /// 串行化「快照 + 落盘」的写锁。
    ///
    /// 为什么独立于 `records`：文件 IO 期间不该卡住 `record_of` / `record_count`
    /// 这类读者；而**写者本来就该串行**（否则先取快照的可能后写盘，用旧快照覆盖另一条记录）。
    /// 锁序固定为 `records_write` → `records`；其它路径只单独取 `records`，
    /// 不存在反向获取，故不成环。
    records_write: Mutex<()>,
}

/// 事件缓冲上限。超过就丢最旧的——它是给"轮询取增量"用的，不是审计日志。
const EVENT_LOG_CAPACITY: usize = 2_000;

/// 下载记录里的单条（**带版本字段**，`docs/05` M2 验收标准 ④）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordEntry {
    pub state: JobState,
    pub dest_path: String,
    pub bytes: u64,
    /// 源地址。**重启续传要靠它**——只记"下到哪了"是不够的，还得知道"从哪下"。
    #[serde(default)]
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
}

/// 记录文件格式。**版本字段是必需的**：格式一定会演进，没有版本就没法安全迁移。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordsFile {
    pub version: u32,
    pub jobs: HashMap<String, RecordEntry>,
}

pub const RECORDS_VERSION: u32 = 1;

/// 下载队列。
pub struct DownloadQueue {
    config: QueueConfig,
    inner: Arc<Inner>,
    permits: Arc<Semaphore>,
    http: HttpDownloader,
    aria2: Option<Arc<Aria2Next>>,
    /// **当前**代理。可运行中更换（`net.set_proxy` 的语义就是"不必重启进程"）。
    ///
    /// 存在这里而不是只留在 `config` 里，是因为 `config` 在 `start` 之后不可变，
    /// 而代理会变——实测同一天内端口换了三次（`AGENTS.md` 踩坑记录 8）。
    proxy: Arc<std::sync::RwLock<ProxyConfig>>,
}

impl std::fmt::Debug for DownloadQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DownloadQueue")
            .field("concurrency", &self.config.limits.cdn_concurrency)
            .field("aria2", &self.aria2.is_some())
            .finish_non_exhaustive()
    }
}

impl DownloadQueue {
    /// 构造队列。配置了 Aria2Next 就会**真的把它启动起来**（起不来则报错，不静默降级）。
    pub async fn start(config: QueueConfig) -> Result<Self, XError> {
        let http = HttpDownloader::new(&config.proxy, config.limits)?;
        let aria2 = match &config.aria2 {
            Some(cfg) => {
                Some(Arc::new(Aria2Next::spawn(cfg.clone()).await.map_err(
                    |e| XError::transport("aria2_spawn", e.to_string()),
                )?))
            }
            None => None,
        };
        let concurrency = config.limits.sanitized().cdn_concurrency.max(1);
        let (events, _) = broadcast::channel(EVENT_LOG_CAPACITY);
        let inner = Arc::new(Inner {
            jobs: Mutex::new(HashMap::new()),
            events,
            log: Mutex::new((0, Vec::new())),
            records: Mutex::new(HashMap::new()),
            records_write: Mutex::new(()),
        });

        let queue = Self {
            permits: Arc::new(Semaphore::new(concurrency as usize)),
            http,
            aria2,
            inner,
            proxy: Arc::new(std::sync::RwLock::new(config.proxy.clone())),
            config,
        };

        // 重启对账：把上次没下完的任务恢复成 waiting，并重新派发（续传）
        let restored = queue.load_records();
        if restored > 0 {
            tracing::info!(count = restored, "从下载记录恢复了未完成任务");
        }
        Ok(queue)
    }

    /// 并发上限（自检项：外壳可以据此核对"我配的和我拿到的是不是一回事"）。
    pub fn concurrency_limit(&self) -> u32 {
        self.config.limits.sanitized().cdn_concurrency.max(1)
    }

    /// Aria2Next 的版本（没配就没有）。启动自检项之一。
    pub fn aria2_version(&self) -> Option<&str> {
        self.aria2.as_ref().map(|a| a.version())
    }

    pub fn records_path(&self) -> Option<&Path> {
        self.config.records_path.as_deref()
    }

    /// 换代理。两个后端**一起换**：
    /// 内置的后端重建客户端，外派的后端逐任务带上新代理。
    ///
    /// 不做这一步的后果很具体：`net.set_proxy` 改了代理、取数立刻好了，
    /// 而下载还在走旧出口（或旧端口已经不可达）——表现为"能刷出列表但一个都下不动"。
    pub fn set_proxy(&self, proxy: ProxyConfig) -> XResult<()> {
        self.http.set_proxy(&proxy)?;
        *self.proxy.write().expect("proxy rwlock poisoned") = proxy;
        Ok(())
    }

    /// 当前代理（自检用）。
    pub fn proxy(&self) -> ProxyConfig {
        self.proxy.read().expect("proxy rwlock poisoned").clone()
    }

    /// 订阅事件流（进程内形态）。HTTP / stdio 形态请用 [`Self::events_since`] 轮询。
    pub fn subscribe(&self) -> broadcast::Receiver<DownloadEvent> {
        self.inner.events.subscribe()
    }

    /// 取某个序号之后的事件。**三种传输都能用**（请求/响应形态不支持流，
    /// 所以这里给一个"带游标的增量接口"，见 `docs/CONTRACT.md` 的 `dl.events`）。
    pub fn events_since(&self, since: u64) -> (u64, Vec<SequencedEvent>) {
        let log = self.inner.log.lock().expect("event log poisoned");
        let (seq, events) = &*log;
        let items = events
            .iter()
            .filter(|e| e.seq > since)
            .cloned()
            .collect::<Vec<_>>();
        (*seq, items)
    }

    /// 入队。**同 `job_id` 第二次入队只算一次**（幂等）。
    pub async fn enqueue(&self, job: EnqueueJob) -> Result<AcceptedBy, XError> {
        if job.url.trim().is_empty() {
            return Err(XError::invalid_request("url 不能为空"));
        }
        let key = job.job_id.to_string();

        {
            let jobs = self.inner.jobs.lock().expect("jobs poisoned");
            if jobs.contains_key(&key) {
                // 幂等：跨页重复投递 / 暂停后重试 / 重启对账，三种情况都走这里
                return Ok(AcceptedBy::AlreadyKnown);
            }
        }
        // 重启之后内存里没有任务，但**记录里有**——同样要认幂等，
        // 否则重启一次就会把整批已下 / 在下的任务重新投一遍
        if self.record_of(&key).is_some() {
            return Ok(AcceptedBy::AlreadyKnown);
        }

        // 已下好且一致 → 跳过（不占并发额度、不发请求）。
        //
        // 判据看的是**目标文件本身**，而不是按 job_id 查记录：
        // `docs/02` §E1 说得很清楚——"按文件名"与"按记录文件"是两种不同的
        // 跳过方式，行为也不同（按文件名改个名就失效，按记录则不会）。
        // 这里实现的是"按文件名"这一种：文件在、大小对，就不重下。
        // （同一个 job_id 的重复投递走上面的幂等分支，连这里都不用进。）
        if job.skip_if_present {
            let matches_expectation = match std::fs::metadata(&job.dest_path) {
                Ok(meta) => job
                    .expect_size
                    .map(|expected| meta.len() == expected)
                    // 没给期望大小时只能"文件在就算"——这是外壳的选择（§E1 的用户设置项）
                    .unwrap_or(meta.len() > 0),
                Err(_) => false,
            };
            if matches_expectation {
                let bytes = std::fs::metadata(&job.dest_path)
                    .map(|m| m.len())
                    .unwrap_or(0);
                self.push_event(DownloadEvent::Skipped {
                    job_id: key.clone(),
                    reason: "already_present".to_string(),
                });
                self.remember(JobEntry {
                    state: JobState::Complete,
                    done: bytes,
                    total: bytes,
                    reason: Some("skipped".to_string()),
                    error: None,
                    cancel: CancelToken::new(),
                    intent: CancelIntent::None,
                    engine: None,
                    attempts: 0,
                    epoch: 0,
                    job: job.clone(),
                });
                return Ok(AcceptedBy::Skipped);
            }
        }

        let entry = JobEntry {
            state: JobState::Waiting,
            done: 0,
            total: job.expect_size.unwrap_or(0),
            reason: None,
            error: None,
            cancel: CancelToken::new(),
            intent: CancelIntent::None,
            engine: None,
            attempts: 0,
            epoch: 0,
            job,
        };
        let cancel_token = entry.cancel.clone();
        self.remember(entry);
        self.dispatch(key, cancel_token);
        Ok(AcceptedBy::Queued)
    }

    /// 暂停：**保留断点**，恢复时接着下。
    pub fn pause(&self, job_id: &str) -> Result<(), XError> {
        let mut jobs = self.inner.jobs.lock().expect("jobs poisoned");
        let entry = jobs
            .get_mut(job_id)
            .ok_or_else(|| XError::not_found(format!("未知 job_id：{job_id}")))?;
        match entry.state {
            JobState::Complete | JobState::Error => {
                return Err(XError::invalid_request(format!(
                    "任务已结束（{:?}），不能暂停",
                    entry.state
                )));
            }
            _ => {}
        }
        entry.intent = CancelIntent::Pause;
        // 标记为 Paused **同时**取消在飞传输；半成品保留（后端不删）
        entry.state = JobState::Paused;
        entry.epoch += 1; // 让上一轮任务的结果失效，别来覆盖 Paused
        let token = entry.cancel.clone();
        drop(jobs);
        token.cancel();
        Ok(())
    }

    /// 恢复。
    pub fn resume(&self, job_id: &str) -> Result<(), XError> {
        let token = {
            let mut jobs = self.inner.jobs.lock().expect("jobs poisoned");
            let entry = jobs
                .get_mut(job_id)
                .ok_or_else(|| XError::not_found(format!("未知 job_id：{job_id}")))?;
            if matches!(entry.state, JobState::Complete) {
                return Err(XError::invalid_request("任务已完成，不能恢复"));
            }
            entry.intent = CancelIntent::None;
            entry.state = JobState::Waiting;
            // 换一个新 token：旧的已经取消了
            entry.cancel = CancelToken::new();
            entry.epoch += 1;
            entry.cancel.clone()
        };
        self.dispatch(job_id.to_string(), token);
        Ok(())
    }

    /// 取消：**丢弃断点**，目标目录保持干净。
    pub fn cancel(&self, job_id: &str) -> Result<(), XError> {
        let (token, settle_now, dest) = {
            let mut jobs = self.inner.jobs.lock().expect("jobs poisoned");
            let entry = jobs
                .get_mut(job_id)
                .ok_or_else(|| XError::not_found(format!("未知 job_id：{job_id}")))?;
            if matches!(entry.state, JobState::Complete) {
                return Err(XError::invalid_request("任务已完成，不能取消"));
            }
            entry.intent = CancelIntent::Discard;
            // **不要在这里就把状态标成 Error**：字节还在流，
            // "已取消"应当由真正停下来的那一刻来落（否则外壳会看到
            // "状态已终态、但文件还在写"这种自相矛盾的快照）。
            let in_flight = matches!(entry.state, JobState::Active);
            if !in_flight {
                entry.state = JobState::Error;
                entry.reason = Some("cancelled".to_string());
                entry.error = Some(XError::Cancelled.to_object());
            }
            (
                entry.cancel.clone(),
                !in_flight,
                entry.job.dest_path.clone(),
            )
        };
        token.cancel();
        if settle_now {
            // 没在飞的任务不会有人来收尾，这里自己收
            cleanup_partials(&dest);
            self.push_event(DownloadEvent::Failed {
                job_id: job_id.to_string(),
                reason: "cancelled".to_string(),
                error: Box::new(XError::Cancelled.to_object()),
            });
        }
        Ok(())
    }

    pub fn status(&self, job_id: &str) -> Option<JobSnapshot> {
        self.inner
            .jobs
            .lock()
            .expect("jobs poisoned")
            .get(job_id)
            .map(JobEntry::snapshot)
    }

    /// 列出所有任务（外壳用它做重启后对账）。
    pub fn list(&self) -> Vec<JobSnapshot> {
        let jobs = self.inner.jobs.lock().expect("jobs poisoned");
        let mut out: Vec<JobSnapshot> = jobs.values().map(JobEntry::snapshot).collect();
        // 稳定顺序：外壳做 diff 时不用自己排
        out.sort_by(|a, b| a.job_id.cmp(&b.job_id));
        out
    }

    /// 清掉已结束的任务（不给内存漏；外壳调不调都行）。
    pub fn prune_finished(&self) {
        let mut jobs = self.inner.jobs.lock().expect("jobs poisoned");
        jobs.retain(|_, e| !matches!(e.state, JobState::Complete | JobState::Error));
    }

    /// 关停：取消所有在飞任务，并优雅关掉 Aria2Next（不留子进程）。
    pub async fn shutdown(&self) {
        {
            let jobs = self.inner.jobs.lock().expect("jobs poisoned");
            for entry in jobs.values() {
                entry.cancel.cancel();
            }
        }
        // 等一小会儿让在飞任务收尾（它们持有 permit）
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while self.permits.available_permits() < self.config.limits.cdn_concurrency as usize {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await;
        // 引擎关停分两种情况：
        // - 在飞任务都已结束（上面的等待）→ Arc 引用数归 1 → 可以**优雅关停**（走 aria2.shutdown）；
        // - 仍有引用（异常路径）→ 交给 Drop，`kill_on_drop` 会杀掉它，**绝不留残留进程**。
        if let Some(arc) = self.aria2.clone() {
            match Arc::try_unwrap(arc) {
                Ok(mut engine) => {
                    if let Err(e) = engine.shutdown().await {
                        tracing::warn!(error = %e, "Aria2Next 优雅关停失败（Drop 会兜底）");
                    }
                }
                Err(_) => {
                    tracing::debug!("Aria2Next 仍被引用，交由 Drop 关停");
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // 内部
    // ------------------------------------------------------------------

    /// 派发一个任务：拿并发额度 → 跑 → 落状态。并发由信号量保证。
    fn dispatch(&self, job_id: String, cancel: CancelToken) {
        let inner = self.inner.clone();
        let permits = self.permits.clone();
        let http = self.http.clone();
        let aria2 = self.aria2.clone();
        let config = self.config.clone();
        let proxy = self.proxy.clone();
        let queue_events = self.inner.clone();

        let epoch = {
            let mut jobs = self.inner.jobs.lock().expect("jobs poisoned");
            match jobs.get_mut(&job_id) {
                Some(entry) => {
                    entry.epoch += 1;
                    entry.epoch
                }
                None => return,
            }
        };

        tokio::spawn(async move {
            // 并发上限：拿不到额度就在这里等（状态保持 waiting）
            let Ok(permit) = permits.acquire_owned().await else {
                return;
            };
            run_job(
                inner,
                queue_events,
                http,
                aria2,
                config,
                proxy,
                job_id,
                epoch,
                cancel,
                permit,
            )
            .await;
        });
    }

    fn remember(&self, entry: JobEntry) {
        self.inner
            .jobs
            .lock()
            .expect("jobs poisoned")
            .insert(entry.job.job_id.to_string(), entry);
    }

    fn push_event(&self, event: DownloadEvent) {
        // 广播给进程内订阅者（没人听也不算错）
        let _ = self.inner.events.send(event.clone());
        let mut log = self.inner.log.lock().expect("event log poisoned");
        let seq = log.0 + 1;
        log.0 = seq;
        if log.1.len() >= EVENT_LOG_CAPACITY {
            log.1.remove(0);
        }
        log.1.push(SequencedEvent { seq, event });
    }

    fn record_of(&self, job_id: &str) -> Option<RecordEntry> {
        self.inner
            .records
            .lock()
            .expect("records poisoned")
            .get(job_id)
            .cloned()
    }

    /// 写入一条记录并落盘。**运行时路径**在 `write_record_shared`（跑完任务时用），
    /// 这里留一个同语义的入口给测试与"外壳要求补记录"的场景。
    #[cfg(test)]
    fn write_record(&self, job_id: &str, entry: RecordEntry) {
        write_record_shared(&self.inner, &self.config, job_id, entry);
    }

    /// 读记录，把**未完成**的任务恢复成 waiting 并重新派发（重启续传）。
    fn load_records(&self) -> usize {
        let Some(path) = &self.config.records_path else {
            return 0;
        };
        let Ok(text) = std::fs::read_to_string(path) else {
            return 0;
        };
        let file: RecordsFile = match serde_json::from_str(&text) {
            Ok(file) => file,
            Err(e) => {
                // 记录坏了不能让它把队列带崩：记一条 warn 就当没有
                tracing::warn!(path = %path.display(), error = %e, "下载记录无法解析，忽略");
                return 0;
            }
        };
        if file.version != RECORDS_VERSION {
            tracing::warn!(
                found = file.version,
                expected = RECORDS_VERSION,
                "下载记录版本不匹配，忽略（不猜测旧格式）"
            );
            return 0;
        }
        *self.inner.records.lock().expect("records poisoned") = file.jobs.clone();

        // 恢复未完成的任务：**立刻重新排队**（续传），而不是等外壳来叫。
        // 已完成的记录留着供幂等与 skip_if_present 判定；
        // 失败（Error）的**不自动重试**——重试与否是外壳的决定。
        let mut restored = 0;
        for (job_id, record) in &file.jobs {
            if matches!(record.state, JobState::Complete | JobState::Error) {
                continue;
            }
            if record.url.trim().is_empty() {
                tracing::warn!(job_id, "记录里没有 url，无法恢复（旧格式？）");
                continue;
            }
            let mut job = EnqueueJob::new(job_id.clone(), record.url.clone(), &record.dest_path);
            job.expect_size = record.expect_size;
            job.tag = record.tag.clone();
            restored += 1;
            self.remember(JobEntry {
                state: JobState::Waiting,
                done: 0,
                total: record.expect_size.unwrap_or(0),
                reason: None,
                error: None,
                cancel: CancelToken::new(),
                intent: CancelIntent::None,
                engine: None,
                attempts: 0,
                epoch: 0,
                job,
            });
            let token = self
                .inner
                .jobs
                .lock()
                .expect("jobs poisoned")
                .get(job_id)
                .map(|e| e.cancel.clone())
                .unwrap_or_default();
            self.dispatch(job_id.clone(), token);
        }
        restored
    }

    /// 把恢复出来的未完成任务重新排队（由 `start()` 调用后由外壳触发，或自动调用）。
    pub async fn requeue_interrupted(&self, jobs: Vec<EnqueueJob>) -> usize {
        let mut count = 0;
        for job in jobs {
            if self.enqueue(job).await == Ok(AcceptedBy::Queued) {
                count += 1;
            }
        }
        count
    }
}

/// 跑一个任务：调用后端 → 落状态 → 发事件 → 写记录。
#[allow(clippy::too_many_arguments)]
async fn run_job(
    inner: Arc<Inner>,
    events: Arc<Inner>,
    http: HttpDownloader,
    aria2: Option<Arc<Aria2Next>>,
    config: QueueConfig,
    proxy: Arc<std::sync::RwLock<ProxyConfig>>,
    job_id: String,
    epoch: u64,
    cancel: CancelToken,
    _permit: OwnedSemaphorePermit,
) {
    // 大小未知时先问服务端——**引擎选择要用它**（小文件走内置、大文件走 aria2），
    // 而且拿到大小之后 `expect_size` 才有值，完整性校验才成立。
    let job = {
        let needs_probe = {
            let jobs = inner.jobs.lock().expect("jobs poisoned");
            match jobs.get(&job_id) {
                Some(entry) if entry.epoch == epoch => {
                    entry.job.expect_size.is_none() && config.probe_size_when_unknown
                }
                _ => return,
            }
        };
        let mut job = {
            let jobs = inner.jobs.lock().expect("jobs poisoned");
            match jobs.get(&job_id) {
                Some(entry) => entry.job.clone(),
                None => return,
            }
        };
        if needs_probe {
            match http.probe_size(&job.url, &cancel).await {
                Ok(Some(size)) => {
                    tracing::debug!(job_id, size, "探到真实大小，用它做完整性校验");
                    job.expect_size = Some(size);
                    let mut jobs = inner.jobs.lock().expect("jobs poisoned");
                    if let Some(entry) = jobs.get_mut(&job_id) {
                        if entry.epoch == epoch {
                            entry.job.expect_size = Some(size);
                            entry.total = size;
                        }
                    }
                }
                // 探测失败不是错误：服务端没说就按"未知"继续（docs/02 §E2 的
                // Unverified 分支就是为这种情况留的）
                Ok(None) => tracing::debug!(job_id, "服务端没给大小，按未知处理"),
                Err(e) => tracing::debug!(job_id, error = %e, "大小探测失败，按未知处理"),
            }
        }
        job
    };

    // 取任务快照
    let (job, engine) = {
        let mut jobs = inner.jobs.lock().expect("jobs poisoned");
        let Some(entry) = jobs.get_mut(&job_id) else {
            return;
        };
        // 已经被新的一轮取代，或者排队期间被暂停/取消了 → 这一轮什么都不做
        if entry.epoch != epoch || !matches!(entry.state, JobState::Waiting) {
            return;
        }
        let engine = choose_engine_static(&aria2, &config, &job);
        entry.state = JobState::Active;
        entry.engine = Some(engine);
        entry.attempts += 1;
        (job, engine)
    };

    // 队列总是让后端保留半成品：删不删由队列决定（暂停 vs 放弃）
    let request = DownloadRequest {
        keep_partial_on_cancel: true,
        // 分片数直达外派后端（内置后端用不上多连接，会忽略它）
        segments: job.requirements.segments,
        // 代理**在派发这一刻**取，而不是队列创建那一刻：代理会变
        proxy_url: proxy.read().expect("proxy rwlock poisoned").resolve_url(),
        ..DownloadRequest::new(job.url.clone(), job.dest_path.clone())
    };
    let request = match job.expect_size {
        Some(size) => request.with_expect_size(size),
        None => request,
    };

    // 进度：直接写进任务状态并广播（调用方要节流是调用方的事）
    let progress_inner = inner.clone();
    let progress_events = events.clone();
    let progress_job = job_id.clone();
    let mut on_progress = move |done: u64, total: u64| {
        {
            let mut jobs = progress_inner.jobs.lock().expect("jobs poisoned");
            if let Some(entry) = jobs.get_mut(&progress_job) {
                // 单调不减：续传/重试时回退不算退步
                entry.done = entry.done.max(done);
                if total > 0 {
                    entry.total = total;
                }
            }
        }
        push_shared(
            &progress_events,
            DownloadEvent::Progress {
                job_id: progress_job.clone(),
                done,
                total,
            },
        );
    };

    let result = match engine {
        EngineChoice::Http => {
            http.download_with_progress(&request, &cancel, &mut on_progress)
                .await
        }
        EngineChoice::Aria2Next => match &aria2 {
            Some(engine) => {
                engine
                    .download_with_progress(&request, &cancel, &mut on_progress)
                    .await
            }
            None => Err(DownloadError::Invalid {
                message: "选择了 Aria2Next 但引擎不可用（内部状态不一致）".to_string(),
            }),
        },
    };

    finish_job(inner, events, config, job_id, epoch, job.dest_path, result).await;
}

/// 引擎选择（与 [`DownloadQueue::choose_engine`] 同一规则；抽出来是为了能在 spawn 后使用）。
fn choose_engine_static(
    aria2: &Option<Arc<Aria2Next>>,
    config: &QueueConfig,
    job: &EnqueueJob,
) -> EngineChoice {
    if aria2.is_none() {
        return EngineChoice::Http;
    }
    let wants_multi = job.requirements.segments > 1;
    let big_or_unknown = job
        .expect_size
        .map(|size| size >= config.min_size_for_aria2)
        .unwrap_or(true);
    if wants_multi || big_or_unknown {
        EngineChoice::Aria2Next
    } else {
        EngineChoice::Http
    }
}

async fn finish_job(
    inner: Arc<Inner>,
    events: Arc<Inner>,
    config: QueueConfig,
    job_id: String,
    epoch: u64,
    dest_path: PathBuf,
    result: Result<DownloadOutcome, DownloadError>,
) {
    // 只在自己那一轮里落状态：被暂停/恢复/取消过之后，上一轮的结果就作废了
    let (still_mine, intent) = {
        let jobs = inner.jobs.lock().expect("jobs poisoned");
        match jobs.get(&job_id) {
            Some(entry) => (entry.epoch == epoch, entry.intent),
            None => return,
        }
    };
    if !still_mine {
        tracing::debug!(job_id, epoch, "这一轮已被取代，丢弃结果");
        return;
    }

    match result {
        Ok(outcome) => {
            {
                let mut jobs = inner.jobs.lock().expect("jobs poisoned");
                if let Some(entry) = jobs.get_mut(&job_id) {
                    entry.state = JobState::Complete;
                    entry.done = outcome.bytes;
                    entry.total = outcome.bytes.max(entry.total);
                    entry.error = None;
                    entry.reason = None;
                    entry.intent = CancelIntent::None;
                }
            }
            write_record_shared(
                &inner,
                &config,
                &job_id,
                RecordEntry {
                    state: JobState::Complete,
                    dest_path: dest_path.display().to_string(),
                    bytes: outcome.bytes,
                    completed_at: Some(xspider_core::xdate::today_utc()),
                    ..record_for(&inner, &job_id)
                },
            );
            push_shared(
                &events,
                DownloadEvent::Completed {
                    job_id,
                    path: dest_path.display().to_string(),
                    bytes: outcome.bytes,
                    integrity: outcome.integrity,
                },
            );
        }
        Err(error) => {
            // 暂停是一种**状态**，不是失败：不发 Failed 事件，也不动断点
            if error == DownloadError::Cancelled && intent == CancelIntent::Pause {
                tracing::debug!(job_id, "已暂停（保留断点）");
                return;
            }
            let (state, reason) = classify_failure(&error);
            {
                let mut jobs = inner.jobs.lock().expect("jobs poisoned");
                if let Some(entry) = jobs.get_mut(&job_id) {
                    entry.state = state;
                    entry.reason = Some(reason.clone());
                    entry.error = Some(error.clone().into());
                    entry.intent = CancelIntent::None;
                }
            }
            // 半成品处理：暂停要留（上面已经 return）；其它失败里只有"可能是暂时的"
            // 才值得留着给下次续传——完整性失败与 404 留着毫无意义
            if !error.is_resumable() {
                cleanup_partials(&dest_path);
            }
            push_shared(
                &events,
                DownloadEvent::Failed {
                    job_id,
                    reason,
                    error: Box::new(error.into()),
                },
            );
        }
    }
}

/// 失败 → (状态, 结构化原因标签)。**不匹配文案**。
fn classify_failure(error: &DownloadError) -> (JobState, String) {
    let reason = match error {
        DownloadError::Cancelled => "cancelled",
        DownloadError::NotFound => "not_found",
        DownloadError::AuthRequired { .. } => "auth_required",
        DownloadError::IntegrityFailed { .. } => "integrity_failed",
        DownloadError::Truncated { .. } => "truncated",
        DownloadError::Transport { .. } => "transport",
        DownloadError::Io { kind, .. } => kind,
        DownloadError::Upstream { .. } => "upstream",
        DownloadError::Invalid { .. } => "invalid",
    };
    // 取消是"被要求停下"，归到 Error 并带 cancelled 码：契约的任务状态只有五个
    // （waiting/active/paused/error/complete），"被取消"用 error.code 区分，
    // 不给 Shell 多一个状态去处理（见 DECISIONS 的 ADR-028）。
    (JobState::Error, reason.to_string())
}

/// 清掉两个后端可能留下的半成品。
fn cleanup_partials(dest: &Path) {
    for attribution in [ATTRIBUTION_HTTP, ATTRIBUTION_ARIA2] {
        let part = part_path_for(dest, attribution);
        let _ = std::fs::remove_file(&part);
        let mut control = part
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        control.push_str(".aria2");
        let _ = std::fs::remove_file(part.with_file_name(control));
    }
}

/// 从队列里取这条任务的 url / expect_size（写记录时要带上，重启续传靠它）。
fn record_for(inner: &Arc<Inner>, job_id: &str) -> RecordEntry {
    let jobs = inner.jobs.lock().expect("jobs poisoned");
    match jobs.get(job_id) {
        Some(entry) => RecordEntry {
            state: entry.state,
            dest_path: entry.job.dest_path.display().to_string(),
            bytes: entry.done,
            url: entry.job.url.clone(),
            expect_size: entry.job.expect_size,
            completed_at: None,
            tag: entry.job.tag.clone(),
        },
        None => RecordEntry {
            state: JobState::Error,
            dest_path: String::new(),
            bytes: 0,
            url: String::new(),
            expect_size: None,
            completed_at: None,
            tag: None,
        },
    }
}

fn push_shared(inner: &Arc<Inner>, event: DownloadEvent) {
    let _ = inner.events.send(event.clone());
    let mut log = inner.log.lock().expect("event log poisoned");
    let seq = log.0 + 1;
    log.0 = seq;
    if log.1.len() >= EVENT_LOG_CAPACITY {
        log.1.remove(0);
    }
    log.1.push(SequencedEvent { seq, event });
}

/// 唯一临时文件名：`<name>.<pid>.<seq>.tmp`。
///
/// 并发写者**绝不能共用同一个临时文件**——否则可能读到彼此写了一半的内容再 rename。
/// pid 隔离跨进程，进程内自增序号隔离同进程内的并发写者。
fn unique_records_tmp(path: &Path) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "records.json".to_string());
    path.with_file_name(format!("{name}.{}.{}.tmp", std::process::id(), n))
}

fn write_record_shared(inner: &Arc<Inner>, config: &QueueConfig, job_id: &str, entry: RecordEntry) {
    write_record_shared_impl(inner, config, job_id, entry, &|| {});
}

/// `before_persist` 仅测试用：在**持有写锁**、取完快照之后、落盘之前调用，
/// 用来确定性地制造「快照已取、盘还没写」的窗口。生产路径传空闭包。
fn write_record_shared_impl(
    inner: &Arc<Inner>,
    config: &QueueConfig,
    job_id: &str,
    entry: RecordEntry,
    before_persist: &dyn Fn(),
) {
    // 没配记录路径：只更新内存（内存是权威，盘只供重启恢复）。
    let Some(path) = &config.records_path else {
        inner
            .records
            .lock()
            .expect("records poisoned")
            .insert(job_id.to_string(), entry);
        return;
    };

    // 同一把写锁包住「insert + 快照 + 写盘」：否则两个任务同时完成时，
    // 先取快照的可能后写盘，用旧快照覆盖掉另一条记录（重启后丢幂等与断点）。
    let _write = inner.records_write.lock().expect("records write poisoned");
    let file = {
        let mut records = inner.records.lock().expect("records poisoned");
        records.insert(job_id.to_string(), entry);
        RecordsFile {
            version: RECORDS_VERSION,
            jobs: records.clone(),
        }
    }; // <- records 锁在这里就释放，读者不受落盘影响
    before_persist();
    let Ok(text) = serde_json::to_string_pretty(&file) else {
        return;
    };
    let tmp = unique_records_tmp(path);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

impl DownloadError {
    /// 这个失败**留着半成品**还有意义吗（下次续传能救回来）。
    fn is_resumable(&self) -> bool {
        matches!(
            self,
            DownloadError::Transport { .. } | DownloadError::Truncated { .. }
        )
    }
}

impl From<DownloadError> for ErrorObject {
    fn from(e: DownloadError) -> Self {
        XError::from(e).to_object()
    }
}

impl DownloadQueue {
    /// 已完成的任务记录数（测试与排障用）。
    pub fn record_count(&self) -> usize {
        self.inner.records.lock().expect("records poisoned").len()
    }
}

/// 供测试：直接看引擎选择的结果。
pub fn engine_for(aria2_available: bool, config: &QueueConfig, job: &EnqueueJob) -> EngineChoice {
    if !aria2_available {
        return EngineChoice::Http;
    }
    let wants_multi = job.requirements.segments > 1;
    let big_or_unknown = job
        .expect_size
        .map(|size| size >= config.min_size_for_aria2)
        .unwrap_or(true);
    if wants_multi || big_or_unknown {
        EngineChoice::Aria2Next
    } else {
        EngineChoice::Http
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(dir: &Path) -> QueueConfig {
        QueueConfig {
            limits: Limits {
                cdn_concurrency: 2,
                ..Limits::default()
            },
            proxy: ProxyConfig::Off,
            aria2: None,
            records_path: Some(dir.join("records.json")),
            min_size_for_aria2: 1024,
            probe_size_when_unknown: true,
        }
    }

    #[test]
    fn requirements_default_to_resume_and_single_segment() {
        let r = Requirements::default();
        assert!(r.resume);
        assert_eq!(r.segments, 1);
        // serde 默认值也要一致（外壳漏传字段时不能变成 0 分片）
        let parsed: Requirements = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed, r);
    }

    #[test]
    fn engine_selection_prefers_aria2_for_multi_segment_and_big_files() {
        let cfg = QueueConfig {
            min_size_for_aria2: 1024,
            ..QueueConfig::default()
        };

        // 多分片 → 必须 aria2（内置后端做不到）
        let mut job = EnqueueJob::new("a", "http://x/y", "/tmp/y");
        job.requirements.segments = 4;
        job.expect_size = Some(10); // 即使很小
        assert_eq!(engine_for(true, &cfg, &job), EngineChoice::Aria2Next);

        // 大文件 → aria2
        let mut job = EnqueueJob::new("b", "http://x/y", "/tmp/y");
        job.expect_size = Some(10_000);
        assert_eq!(engine_for(true, &cfg, &job), EngineChoice::Aria2Next);

        // 小文件且单分片 → 内置（省一个子进程）
        let mut job = EnqueueJob::new("c", "http://x/y", "/tmp/y");
        job.expect_size = Some(10);
        assert_eq!(engine_for(true, &cfg, &job), EngineChoice::Http);

        // 大小未知 → 倾向 aria2（媒体通常是大文件）
        let job = EnqueueJob::new("d", "http://x/y", "/tmp/y");
        assert_eq!(engine_for(true, &cfg, &job), EngineChoice::Aria2Next);

        // 没有 aria2 → 永远内置，且**不报错**（保底路径）
        let mut job = EnqueueJob::new("e", "http://x/y", "/tmp/y");
        job.requirements.segments = 8;
        assert_eq!(engine_for(false, &cfg, &job), EngineChoice::Http);
    }

    #[test]
    fn failure_classification_uses_structured_reasons() {
        assert_eq!(
            classify_failure(&DownloadError::NotFound),
            (JobState::Error, "not_found".to_string())
        );
        assert_eq!(
            classify_failure(&DownloadError::Cancelled),
            (JobState::Error, "cancelled".to_string())
        );
        assert_eq!(
            classify_failure(&DownloadError::IntegrityFailed {
                expected: 1,
                actual: 2
            })
            .1,
            "integrity_failed"
        );
        // io 的 kind 直接作为 reason（disk_full / permission_denied …）
        assert_eq!(
            classify_failure(&DownloadError::Io {
                kind: "disk_full",
                message: String::new()
            })
            .1,
            "disk_full"
        );
    }

    #[test]
    fn only_transient_failures_keep_the_partial_file() {
        assert!(DownloadError::Transport {
            kind: "connect",
            message: String::new()
        }
        .is_resumable());
        // 完整性失败留着半成品毫无意义——留着反而容易被当成"下好了"
        assert!(!DownloadError::IntegrityFailed {
            expected: 1,
            actual: 2
        }
        .is_resumable());
        assert!(!DownloadError::NotFound.is_resumable());
    }

    #[test]
    fn records_file_carries_a_version() {
        let file = RecordsFile {
            version: RECORDS_VERSION,
            jobs: HashMap::new(),
        };
        let json = serde_json::to_value(&file).unwrap();
        assert_eq!(json["version"], 1, "记录格式必须带版本字段");
        assert!(json["jobs"].is_object());
    }

    #[test]
    fn job_snapshot_omits_absent_fields() {
        let snapshot = JobSnapshot {
            job_id: "a".into(),
            state: JobState::Waiting,
            done: 0,
            total: 0,
            reason: None,
            error: None,
            tag: None,
            dest_path: None,
        };
        let json = serde_json::to_value(&snapshot).unwrap();
        for key in ["reason", "error", "tag", "dest_path"] {
            assert!(json.get(key).is_none(), "{key} 不该出现：{json}");
        }
        assert_eq!(json["state"], "waiting");
    }

    #[test]
    fn events_serialize_with_a_kind_tag() {
        let json = serde_json::to_value(DownloadEvent::Skipped {
            job_id: "a".into(),
            reason: "already_present".into(),
        })
        .unwrap();
        assert_eq!(json["kind"], "skipped");
        assert_eq!(json["job_id"], "a");

        let json = serde_json::to_value(DownloadEvent::Progress {
            job_id: "a".into(),
            done: 1,
            total: 2,
        })
        .unwrap();
        assert_eq!(json["kind"], "progress");
    }

    #[tokio::test]
    async fn duplicate_job_id_is_only_accepted_once() {
        // 不真的下载：用一个不可达地址，重点是幂等语义
        let dir = std::env::temp_dir().join(format!("xspider-q-idem-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let queue = DownloadQueue::start(config(&dir)).await.expect("启动队列");

        let job = EnqueueJob::new("job-1", "http://127.0.0.1:1/x", dir.join("x.bin"));
        assert_eq!(
            queue.enqueue(job.clone()).await.unwrap(),
            AcceptedBy::Queued
        );
        assert_eq!(
            queue.enqueue(job).await.unwrap(),
            AcceptedBy::AlreadyKnown,
            "同一个 job_id 第二次入队必须只算一次"
        );
        assert_eq!(queue.list().len(), 1);

        queue.shutdown().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn enqueue_rejects_an_empty_url() {
        let dir = std::env::temp_dir().join(format!("xspider-q-empty-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let queue = DownloadQueue::start(config(&dir)).await.expect("启动队列");
        let err = queue
            .enqueue(EnqueueJob::new("a", "  ", dir.join("x.bin")))
            .await
            .unwrap_err();
        assert_eq!(err.code(), xspider_core::error::ErrorCode::InvalidRequest);
        queue.shutdown().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 代理要在**运行中**能换：`net.set_proxy` 的语义就是"不必重启进程"，
    /// 而实际环境里代理端口一天会变好几次（`AGENTS.md` 踩坑记录 8）。
    /// 这条测试锁住"两个后端一起换"，避免退回"只换取数侧"的半截状态。
    #[tokio::test]
    async fn proxy_can_be_swapped_at_runtime() {
        let dir = std::env::temp_dir().join(format!("xspider-q-proxy-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let queue = DownloadQueue::start(config(&dir)).await.expect("启动队列");
        assert_eq!(queue.proxy(), ProxyConfig::Off);

        queue
            .set_proxy(ProxyConfig::Manual("http://127.0.0.1:17890".into()))
            .expect("换代理应成功");
        assert_eq!(
            queue.proxy().resolve_url(),
            Some("http://127.0.0.1:17890".into()),
            "换个代理之后，逐任务拿到的必须是新值（否则 aria2 还在走旧出口）"
        );

        queue.shutdown().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn unknown_job_id_is_a_structured_not_found() {
        let dir = std::env::temp_dir().join(format!("xspider-q-unknown-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let queue = DownloadQueue::start(config(&dir)).await.expect("启动队列");
        for action in [
            queue.pause("nope").map(|_| ()),
            queue.resume("nope").map(|_| ()),
            queue.cancel("nope").map(|_| ()),
        ] {
            let err = action.unwrap_err();
            assert_eq!(err.code(), xspider_core::error::ErrorCode::NotFound);
        }
        queue.shutdown().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn events_are_retrievable_by_cursor() {
        let dir = std::env::temp_dir().join(format!("xspider-q-events-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let queue = DownloadQueue::start(config(&dir)).await.expect("启动队列");

        let (seq0, events0) = queue.events_since(0);
        assert_eq!(seq0, 0);
        assert!(events0.is_empty());

        // 造一个"目标文件已存在且大小对"的跳过场景（按文件名判定，docs/02 §E1）
        let dest = dir.join("present.bin");
        std::fs::write(&dest, b"12345").unwrap();
        let mut job = EnqueueJob::new("job-skip", "http://127.0.0.1:1/x", &dest);
        job.skip_if_present = true;
        job.expect_size = Some(5);
        assert_eq!(queue.enqueue(job).await.unwrap(), AcceptedBy::Skipped);

        // 大小对不上就不能跳（否则会把一个残缺文件当成"已下好"）
        let dest2 = dir.join("wrong-size.bin");
        std::fs::write(&dest2, b"123").unwrap();
        let mut job2 = EnqueueJob::new("job-skip-2", "http://127.0.0.1:1/x", &dest2);
        job2.skip_if_present = true;
        job2.expect_size = Some(5);
        assert_eq!(
            queue.enqueue(job2).await.unwrap(),
            AcceptedBy::Queued,
            "大小不符时必须重下，而不是当成功"
        );

        let (seq1, events1) = queue.events_since(0);
        assert!(seq1 >= 1);
        assert!(
            events1
                .iter()
                .any(|e| matches!(e.event, DownloadEvent::Skipped { .. })),
            "应当有 skipped 事件：{events1:?}"
        );
        // 再取一次增量：没有新的
        let (_, events2) = queue.events_since(1);
        assert!(events2.is_empty());

        queue.shutdown().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn records_survive_a_restart_and_report_the_version() {
        let dir = std::env::temp_dir().join(format!("xspider-q-restart-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        {
            let queue = DownloadQueue::start(config(&dir)).await.expect("启动队列");
            queue.write_record(
                "job-done",
                RecordEntry {
                    state: JobState::Complete,
                    dest_path: dir.join("done.bin").display().to_string(),
                    bytes: 42,
                    url: "http://example.invalid/x".to_string(),
                    expect_size: Some(42),
                    completed_at: Some("2026-10-01".to_string()),
                    tag: Some("post-1".to_string()),
                },
            );
            queue.shutdown().await;
        }
        // 新进程/新队列：记录要能读回来，并带版本字段
        let queue = DownloadQueue::start(config(&dir)).await.expect("重启队列");
        assert_eq!(queue.record_count(), 1);
        let raw = std::fs::read_to_string(dir.join("records.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed["version"], 1);
        assert_eq!(parsed["jobs"]["job-done"]["bytes"], 42);
        assert_eq!(parsed["jobs"]["job-done"]["state"], "complete");
        assert_eq!(parsed["jobs"]["job-done"]["tag"], "post-1");
        queue.shutdown().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_corrupt_records_file_does_not_break_the_queue() {
        let dir = std::env::temp_dir().join(format!("xspider-q-corrupt-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("records.json"), b"{not json").unwrap();
        // 记录坏了只该 warn，不该让队列起不来
        let queue = DownloadQueue::start(config(&dir))
            .await
            .expect("队列必须能起来");
        assert_eq!(queue.record_count(), 0);
        queue.shutdown().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_records_file_with_a_future_version_is_ignored_not_guessed() {
        let dir = std::env::temp_dir().join(format!("xspider-q-version-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(
            dir.join("records.json"),
            br#"{"version": 99, "jobs": {"a": {"state":"complete","dest_path":"/x","bytes":1}}}"#,
        )
        .unwrap();
        let queue = DownloadQueue::start(config(&dir))
            .await
            .expect("队列必须能起来");
        assert_eq!(
            queue.record_count(),
            0,
            "版本不认识就该忽略，而不是按当前格式硬读"
        );
        queue.shutdown().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn record_entry(job_id: &str) -> RecordEntry {
        RecordEntry {
            state: JobState::Complete,
            dest_path: format!("/tmp/{job_id}.bin"),
            bytes: 1,
            url: format!("http://example.invalid/{job_id}"),
            expect_size: Some(1),
            completed_at: Some("2026-10-01".to_string()),
            tag: None,
        }
    }

    /// 并发写者绝不能共用同一个临时文件（会撕裂）。
    #[test]
    fn records_tmp_files_are_unique_per_writer() {
        let p = Path::new("/tmp/xspider/records.json");
        let a = unique_records_tmp(p);
        let b = unique_records_tmp(p);
        assert_ne!(a, b, "两个写者的临时文件名必须不同");
        assert_eq!(a.parent(), Some(Path::new("/tmp/xspider")));
        assert!(
            a.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("records.json."),
            "临时名应当从记录名派生：{}",
            a.display()
        );
        assert!(a.to_string_lossy().ends_with(".tmp"));
    }

    /// **快照 + 落盘必须在同一把写锁里**。
    ///
    /// 反例（修前）：先 insert 解锁，再另外取锁 clone 整表，然后在锁外写盘——
    /// A 取完快照停在这里，B 插进来写入自己的记录，A 再把自己的旧快照写回去，
    /// **B 的记录就从盘上消失了**（重启后丢幂等与断点）。
    ///
    /// 这条用 `before_persist` 钩子在「快照已取、盘还没写」的窗口里插入并发写者，
    /// 确定性地逼出那个交错：修好后 A 全程持有写锁，B 只能排在后面，
    /// 它取到的快照已经包含 A，最终两条都在。
    #[tokio::test]
    async fn snapshot_and_persist_are_serialized_so_no_record_is_lost() {
        let dir = std::env::temp_dir().join(format!("xspider-q-atomic-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let queue = DownloadQueue::start(config(&dir)).await.expect("启动队列");
        let inner = queue.inner.clone();
        let cfg = queue.config.clone();

        let (snapshot_taken_tx, snapshot_taken_rx) = std::sync::mpsc::channel::<()>();
        let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();

        let inner_a = inner.clone();
        let cfg_a = cfg.clone();
        let a = std::thread::spawn(move || {
            write_record_shared_impl(&inner_a, &cfg_a, "job-a", record_entry("job-a"), &|| {
                let _ = snapshot_taken_tx.send(());
                let _ = go_rx.recv();
            });
        });

        snapshot_taken_rx
            .recv()
            .expect("A 应当取完快照并停在落盘前");
        let inner_b = inner.clone();
        let cfg_b = cfg.clone();
        let b = std::thread::spawn(move || {
            write_record_shared(&inner_b, &cfg_b, "job-b", record_entry("job-b"));
        });
        // B 尝试拿写锁的时间窗；即便它还没到，最终文件也必须两条都有。
        std::thread::sleep(std::time::Duration::from_millis(60));
        let _ = go_tx.send(());

        a.join().unwrap();
        b.join().unwrap();

        let raw = std::fs::read_to_string(dir.join("records.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(
            parsed["jobs"]["job-a"].is_object(),
            "A 的记录必须落盘：{raw}"
        );
        assert!(
            parsed["jobs"]["job-b"].is_object(),
            "B 的记录不许被 A 的旧快照覆盖：{raw}"
        );
        assert_eq!(queue.record_count(), 2);

        queue.shutdown().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cleanup_removes_partials_from_both_engines() {
        let dir = std::env::temp_dir().join(format!("xspider-q-clean-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let dest = dir.join("x.bin");
        let http_part = part_path_for(&dest, ATTRIBUTION_HTTP);
        let aria_part = part_path_for(&dest, ATTRIBUTION_ARIA2);
        let aria_control = aria_part.with_file_name(format!(
            "{}.aria2",
            aria_part.file_name().unwrap().to_string_lossy()
        ));
        for path in [&http_part, &aria_part, &aria_control] {
            std::fs::write(path, b"x").unwrap();
        }
        cleanup_partials(&dest);
        for path in [&http_part, &aria_part, &aria_control] {
            assert!(!path.exists(), "{} 应当被清掉", path.display());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
