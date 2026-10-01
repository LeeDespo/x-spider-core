//! # xspider-download
//!
//! 下载与爬取调度组件：翻页、筛选、队列、并发、断点续传、完整性校验、
//! 暂停/恢复/取消、事件上报。
//!
//! ## 当前状态
//!
//! | 项 | 状态 |
//! |---|---|
//! | **`http` 内置后端**（流式落盘 + Range 续传 + 断流重试 + 完整性校验 + 原子 rename） | ✅ 见 [`http_backend::HttpDownloader`] |
//! | 契约类型（[`JobId`] / [`JobState`] / [`DoneReason`]） | ✅ 形状已定 |
//! | 队列、并发上限、任务状态机、`job_id` 幂等 | ✅ 见 [`queue::DownloadQueue`] |
//! | 事件上报（progress / completed / failed / skipped） | ✅ [`queue::DownloadEvent`] |
//! | 下载记录（由组件写、带版本字段） | ✅ [`queue::RecordsFile`]（version=1） |
//! | 爬取调度（候选清单 + 策略参数 + `done_reason`） | ✅ 见 [`crawl::crawl`] |
//! | `aria2` 外派后端（**Aria2Next**：子进程 + JSON-RPC） | ✅ 见 [`aria2::Aria2Next`] |
//! | `host` 逃生舱（`dl.plan` + `dl.report`） | ⬜ 待做 |
//!
//! **为什么先把内置 HTTP 后端做出来**（`docs/01-ARCHITECTURE.md` §5.2）：
//! 它是"没有 aria2 二进制也能工作"的保底，也是**离线测试的唯一载体**——
//! 本地 HTTP server 能造出 Range / 断流 / 慢速 / 404 / 大小不符，
//! 而真实 CDN 造不出来（`docs/04` §5）。先有它，M2 的队列才有东西可测。
//!
//! ## 两条从实测里来的纪律（实现时别丢）
//!
//! - **同 `job_id` 重复入队只算一次**：一次解决「跨页重复投递 / 暂停后重试 / 重启对账」
//!   三件事（`docs/01-ARCHITECTURE.md` §5.1）；
//! - **完成判据是完整性校验，不是"任务返回成功"**：对不可达 URL 的实测表现是
//!   留下 0 字节文件，且失败退出码不止一种，所以不能只看退出码（`docs/02` §E2）。

use serde::{Deserialize, Serialize};

pub mod aria2;
pub mod crawl;
pub mod http_backend;
pub mod queue;

pub use aria2::{Aria2Next, Aria2NextConfig};
pub use crawl::{
    crawl, Candidate, CrawlEvent, CrawlLimits, CrawlMedia, CrawlOutcome, CrawlPost, CrawlStats,
    CrawlStrategy, MediaKind, PageSource,
};
pub use http_backend::{
    part_path, part_path_for, DownloadError, DownloadOutcome, DownloadRequest, DownloadResult,
    HttpDownloader, Integrity, ATTRIBUTION_ARIA2, ATTRIBUTION_HTTP,
};
pub use queue::{
    AcceptedBy, DownloadEvent, DownloadQueue, EnqueueJob, JobSnapshot, QueueConfig, RecordEntry,
    RecordsFile, Requirements, SequencedEvent, RECORDS_VERSION,
};

/// 外壳生成的下载任务 id，组件按它幂等。
///
/// 外壳要用它把进度映射回"哪条推文的哪张图"，所以**由外壳生成并传入**
/// （`docs/01-ARCHITECTURE.md` §5.1）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JobId(pub String);

impl JobId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for JobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// 任务状态机（跨端一致，由组件唯一持有）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Waiting,
    Active,
    Paused,
    Error,
    Complete,
}

/// 爬取循环的终止原因（`docs/01-ARCHITECTURE.md` §4）。
///
/// **必须显式**：「翻到服务端尽头」与「被策略提前终止」是两种语义，
/// 外壳靠它决定文案与下次是否续爬。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DoneReason {
    /// 翻到服务端尽头。
    Exhausted,
    /// 客户端筛选的时间轴已经推进到 `since` 之前（docs/02 §D2：这才是主判据）。
    TimeProgressed,
    /// 连续空页达到上限（**只能当辅助判据**，否则停更账号会被误判）。
    EmptyPages,
    /// 游标没有推进（X 偶发回吐同一游标）→ 当作到底，防止原地空转刷爆配额（docs/02 §B8）。
    CursorStuck,
    /// `wanted_keys` 收齐 → 提前终止，省请求（成本相关，所以进契约）。
    WantedCollected,
    /// 触到**调用方给的** `max_pages` 上限而停。
    ///
    /// 与 `cursor_stuck` 分开是刻意的：前者是"我们自己设的闸"，
    /// 后者是"服务端回吐了同一个游标"。混在一起，外壳就没法区分
    /// "还能继续爬（把上限调大即可）"与"这条链路坏了"。
    PageLimitReached,
    /// 调用方取消。
    Cancelled,
    /// 出错终止。
    Error,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_id_is_transparent_in_json() {
        let id = JobId::new("post-1-media-2");
        assert_eq!(serde_json::to_string(&id).unwrap(), r#""post-1-media-2""#);
        assert_eq!(
            serde_json::from_str::<JobId>("\"x\"").unwrap().as_str(),
            "x"
        );
    }

    #[test]
    fn states_use_snake_case_in_the_contract() {
        assert_eq!(
            serde_json::to_string(&JobState::Waiting).unwrap(),
            r#""waiting""#
        );
        assert_eq!(
            serde_json::to_string(&JobState::Complete).unwrap(),
            r#""complete""#
        );
    }

    #[test]
    fn done_reasons_are_a_closed_set_in_the_contract() {
        let all = [
            (DoneReason::Exhausted, "exhausted"),
            (DoneReason::TimeProgressed, "time_progressed"),
            (DoneReason::EmptyPages, "empty_pages"),
            (DoneReason::CursorStuck, "cursor_stuck"),
            (DoneReason::WantedCollected, "wanted_collected"),
            (DoneReason::PageLimitReached, "page_limit_reached"),
            (DoneReason::Cancelled, "cancelled"),
            (DoneReason::Error, "error"),
        ];
        for (reason, expected) in all {
            assert_eq!(
                serde_json::to_string(&reason).unwrap(),
                format!("\"{expected}\""),
                "契约里的 done_reason 枚举值必须稳定"
            );
        }
    }
}
