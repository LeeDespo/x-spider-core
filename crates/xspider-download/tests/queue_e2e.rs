//! **下载队列 E2E**：本地 fixture server + 真队列（并发、暂停/恢复、取消、事件、记录）。
//!
//! 对应 `docs/05-WORKFLOW.md` M2 验收标准里能离线验证的那几条：
//! ① 本地 HTTP E2E（含断点续传）；② `job_id` 幂等与重启恢复；④ 记录由组件写、带版本字段。
//! 「并发上限」也在这一层测——它是队列的唯一致命点（`docs/01` §5.1）。

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::{body_bytes, FixtureServer};
use xspider_core::http::ProxyConfig;
use xspider_core::ratelimit::Limits;
use xspider_download::{
    AcceptedBy, DownloadEvent, DownloadQueue, EnqueueJob, JobState, QueueConfig, Requirements,
};

fn workdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "xspider-qe2e-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("创建临时目录失败");
    dir
}

fn config(dir: &Path, concurrency: u32) -> QueueConfig {
    QueueConfig {
        limits: Limits {
            cdn_concurrency: concurrency,
            ..Limits::default()
        },
        // 本地回环绝不走代理
        proxy: ProxyConfig::Off,
        aria2: None, // 这一组测内置后端；Aria2Next 有自己的 E2E
        records_path: Some(dir.join("records.json")),
        min_size_for_aria2: u64::MAX,
        probe_size_when_unknown: true,
    }
}

/// 轮询直到任务到达终态（或超时）。
async fn wait_for_state(queue: &DownloadQueue, job_id: &str, want: &[JobState]) -> JobState {
    for _ in 0..200 {
        if let Some(snapshot) = queue.status(job_id) {
            if want.contains(&snapshot.state) {
                return snapshot.state;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!(
        "{job_id} 在 10s 内没有到达 {want:?}，当前：{:?}",
        queue.status(job_id)
    );
}

#[tokio::test]
async fn queue_downloads_and_writes_a_record() {
    let server = FixtureServer::start().await;
    let dir = workdir("basic");
    let queue = DownloadQueue::start(config(&dir, 2))
        .await
        .expect("启动队列");
    let mut events = queue.subscribe();

    let dest = dir.join("pic.bin");
    let size = 32 * 1024u64;
    let accepted = queue
        .enqueue(
            EnqueueJob::new("job-1", server.url(&format!("/full?size={size}")), &dest)
                .with_expect_size(size),
        )
        .await
        .expect("入队");
    assert_eq!(accepted, AcceptedBy::Queued);

    wait_for_state(&queue, "job-1", &[JobState::Complete]).await;
    assert_eq!(std::fs::read(&dest).unwrap(), body_bytes(size as usize));

    // 事件：至少有一条 completed，且 progress 单调不减
    let mut completed = false;
    let mut last_done = 0u64;
    for _ in 0..50 {
        match tokio::time::timeout(Duration::from_millis(200), events.recv()).await {
            Ok(Ok(DownloadEvent::Completed { job_id, bytes, .. })) => {
                assert_eq!(job_id, "job-1");
                assert_eq!(bytes, size);
                completed = true;
                break;
            }
            Ok(Ok(DownloadEvent::Progress { done, .. })) => {
                assert!(done >= last_done, "进度必须单调不减：{last_done} → {done}");
                last_done = done;
            }
            Ok(Ok(_)) => {}
            _ => break,
        }
    }
    assert!(completed, "必须收到 completed 事件");

    // 记录：由组件写，带版本字段
    let raw = std::fs::read_to_string(dir.join("records.json")).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(parsed["version"], 1);
    assert_eq!(parsed["jobs"]["job-1"]["state"], "complete");
    assert_eq!(parsed["jobs"]["job-1"]["bytes"], size);

    queue.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// **并发必须真的被限制住**：这是队列存在的理由（`docs/01` §5.1）。
#[tokio::test]
async fn concurrency_is_capped_by_the_configured_limit() {
    let server = FixtureServer::start().await;
    let dir = workdir("concurrency");
    let queue = DownloadQueue::start(config(&dir, 2))
        .await
        .expect("启动队列");

    for index in 0..6 {
        queue
            .enqueue(
                EnqueueJob::new(
                    format!("job-{index}"),
                    server.url("/slow?size=10240"), // 每个约 0.5s
                    dir.join(format!("f{index}.bin")),
                )
                .with_expect_size(10240),
            )
            .await
            .expect("入队");
    }
    for index in 0..6 {
        wait_for_state(&queue, &format!("job-{index}"), &[JobState::Complete]).await;
    }

    assert_eq!(
        queue.concurrency_limit(),
        2,
        "自检：队列拿到的上限应当就是配的 2"
    );
    // 断言"同时**下载中**的请求数"——而不是 socket 计数：
    // TCP 层面还有半关闭/连接池复用的噪音，用 socket 计数会虚高。
    let peak = server.peak_streaming();
    assert!(
        peak <= 2,
        "同时下载数峰值 {peak} 超过了配置上限 2 —— 并发必须有唯一所有者"
    );
    assert_eq!(
        peak, 2,
        "上限是 2 时应当真的跑到 2（否则等于把并发白白浪费）"
    );
    assert_eq!(server.completed_connections(), 6, "6 个任务都该跑完");

    queue.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// **暂停保留断点、恢复接着下**——这条验证的是 `docs/01` §5.1 里"暂停/恢复"的完整语义。
#[tokio::test]
async fn pause_keeps_the_partial_and_resume_finishes_the_download() {
    let server = FixtureServer::start().await;
    let dir = workdir("pause-resume");
    let queue = DownloadQueue::start(config(&dir, 1))
        .await
        .expect("启动队列");
    let size = 20 * 1024u64;
    let dest = dir.join("big.bin");

    queue
        .enqueue(
            EnqueueJob::new("job-p", server.url(&format!("/slow?size={size}")), &dest)
                .with_expect_size(size),
        )
        .await
        .expect("入队");

    // 等它真的开始动（有进度）再暂停，否则可能还没拿到 permit
    let mut started = false;
    for _ in 0..100 {
        if queue.status("job-p").map(|s| s.done).unwrap_or(0) > 0 {
            started = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(started, "任务应当已经开始传输");

    queue.pause("job-p").expect("暂停");
    wait_for_state(&queue, "job-p", &[JobState::Paused]).await;

    let part = xspider_download::part_path(&dest);
    let paused_bytes = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    assert!(
        paused_bytes > 0 && paused_bytes < size,
        "暂停时应当留下**部分**断点（实际 {paused_bytes} 字节）"
    );
    assert!(!dest.exists(), "没下完就不该出现在目标路径");

    // 恢复：应当从断点继续（而不是从 0）
    queue.resume("job-p").expect("恢复");
    wait_for_state(&queue, "job-p", &[JobState::Complete]).await;

    assert_eq!(
        std::fs::read(&dest).unwrap(),
        body_bytes(size as usize),
        "续传拼出来的内容必须逐字节正确"
    );
    assert!(!part.exists(), "完成之后断点文件应当消失");

    // 证据：第二次请求带了 Range（说明真的从断点续传，而不是重下）
    let ranges: Vec<Option<String>> = server.requests().iter().map(|r| r.range.clone()).collect();
    assert!(
        ranges.iter().any(|r| r.is_some()),
        "恢复时必须发出带 Range 的请求，实际：{ranges:?}"
    );

    queue.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// **取消丢弃断点**（与暂停相对）。
#[tokio::test]
async fn cancel_discards_the_partial_and_marks_the_job_failed() {
    let server = FixtureServer::start().await;
    let dir = workdir("cancel");
    let queue = DownloadQueue::start(config(&dir, 1))
        .await
        .expect("启动队列");
    let dest = dir.join("big.bin");

    queue
        .enqueue(EnqueueJob::new(
            "job-c",
            server.url("/slow?size=20480"),
            &dest,
        ))
        .await
        .expect("入队");

    for _ in 0..100 {
        if queue.status("job-c").map(|s| s.done).unwrap_or(0) > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    queue.cancel("job-c").expect("取消");
    wait_for_state(&queue, "job-c", &[JobState::Error]).await;

    let snapshot = queue.status("job-c").unwrap();
    assert_eq!(snapshot.reason.as_deref(), Some("cancelled"));
    assert_eq!(
        snapshot.error.as_ref().map(|e| e.code.as_str()),
        Some("cancelled"),
        "错误码要能让外壳区分「被取消」与「失败」"
    );
    assert!(!dest.exists(), "取消后目标路径必须干净");
    assert!(
        !xspider_download::part_path(&dest).exists(),
        "放弃语义要清掉断点（与暂停相对）"
    );

    queue.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// **重启恢复**：新队列读回记录，对同一个 `job_id` 的再次投递必须是幂等的/可跳过的。
#[tokio::test]
async fn restart_is_reconciled_through_the_records_file() {
    let server = FixtureServer::start().await;
    let dir = workdir("restart");
    let size = 4096u64;
    let dest = dir.join("pic.bin");

    {
        let queue = DownloadQueue::start(config(&dir, 1))
            .await
            .expect("启动队列");
        queue
            .enqueue(
                EnqueueJob::new("job-r", server.url(&format!("/full?size={size}")), &dest)
                    .with_expect_size(size),
            )
            .await
            .expect("入队");
        wait_for_state(&queue, "job-r", &[JobState::Complete]).await;
        queue.shutdown().await;
    }
    let requests_before = server.requests().len();

    // "重启"：新队列读回记录
    let queue = DownloadQueue::start(config(&dir, 1))
        .await
        .expect("重启队列");
    assert_eq!(queue.record_count(), 1, "记录应当被读回来");

    // 再次投递同一个 job_id（重放/重复投递的典型场景）
    let accepted = queue
        .enqueue(
            EnqueueJob::new("job-r", server.url(&format!("/full?size={size}")), &dest)
                .with_expect_size(size),
        )
        .await
        .expect("入队");
    assert_eq!(
        accepted,
        AcceptedBy::AlreadyKnown,
        "重启后重放同一个 job_id 必须被认出来（幂等）"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        server.requests().len(),
        requests_before,
        "幂等命中时**不该再发请求**"
    );

    // skip_if_present：外壳"二次同步"时用它省流量（docs/02 §E1）。
    // 判据是**目标文件本身**（在、且大小对）——所以即使 job_id 是新的也会被跳过。
    let mut skip_job = EnqueueJob::new("job-r2", server.url(&format!("/full?size={size}")), &dest);
    skip_job.skip_if_present = true;
    skip_job.expect_size = Some(size);
    let accepted = queue.enqueue(skip_job).await.expect("入队");
    assert_eq!(
        accepted,
        AcceptedBy::Skipped,
        "文件已在且大小对 → 必须跳过，而不是重下一遍"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        server.requests().len(),
        requests_before,
        "跳过的任务**一个请求都不该发**"
    );

    queue.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// 完整性失败要落进记录与事件，而不是悄悄"成功"。
#[tokio::test]
async fn integrity_failure_surfaces_in_state_events_and_cleanup() {
    let server = FixtureServer::start().await;
    let dir = workdir("integrity");
    let queue = DownloadQueue::start(config(&dir, 1))
        .await
        .expect("启动队列");
    let dest = dir.join("short.bin");

    queue
        .enqueue(
            // 服务端只有 1KB，期望 10KB
            EnqueueJob::new("job-i", server.url("/full?size=1024"), &dest).with_expect_size(10_240),
        )
        .await
        .expect("入队");
    wait_for_state(&queue, "job-i", &[JobState::Error]).await;

    let snapshot = queue.status("job-i").unwrap();
    assert_eq!(snapshot.reason.as_deref(), Some("integrity_failed"));
    assert_eq!(
        snapshot.error.as_ref().map(|e| e.code.as_str()),
        Some("parse"),
        "完整性失败在契约里归 parse 类（拿到的数据不符合预期）"
    );
    assert!(!dest.exists(), "校验失败不该留下文件");

    queue.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// 队列不该被"任务多"拖垮：`prune_finished` 之后列表不会无限增长。
#[tokio::test]
async fn finished_jobs_can_be_pruned() {
    let server = FixtureServer::start().await;
    let dir = workdir("prune");
    let queue = Arc::new(
        DownloadQueue::start(config(&dir, 2))
            .await
            .expect("启动队列"),
    );

    for index in 0..3 {
        queue
            .enqueue(
                EnqueueJob::new(
                    format!("p{index}"),
                    server.url("/full?size=512"),
                    dir.join(format!("p{index}.bin")),
                )
                .with_expect_size(512),
            )
            .await
            .expect("入队");
    }
    for index in 0..3 {
        wait_for_state(&queue, &format!("p{index}"), &[JobState::Complete]).await;
    }
    assert_eq!(queue.list().len(), 3);
    queue.prune_finished();
    assert_eq!(queue.list().len(), 0, "已结束的任务应当可以被清掉");
    assert_eq!(
        queue.record_count(),
        3,
        "但记录要留着——它是「下过了」的依据（docs/02 §E1）"
    );

    queue.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// `requirements.segments > 1` 在没有 Aria2Next 时必须**退化成内置后端**，而不是报错。
#[tokio::test]
async fn multi_segment_requirement_degrades_gracefully_without_aria2() {
    let server = FixtureServer::start().await;
    let dir = workdir("degrade");
    let queue = DownloadQueue::start(config(&dir, 1))
        .await
        .expect("启动队列");
    let dest = dir.join("x.bin");

    let mut job = EnqueueJob::new("job-seg", server.url("/full?size=2048"), &dest);
    job.requirements = Requirements {
        resume: true,
        segments: 8,
    };
    job.expect_size = Some(2048);
    assert_eq!(
        queue.enqueue(job).await.expect("入队"),
        AcceptedBy::Queued,
        "没有 aria2 时多分片要求应当降级，而不是失败"
    );
    wait_for_state(&queue, "job-seg", &[JobState::Complete]).await;
    assert_eq!(std::fs::read(&dest).unwrap(), body_bytes(2048));

    queue.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// **不给 `expect_size` 也能拿到完整性校验**：队列自己去问服务端。
///
/// 这条补的是一个真实缺口：GraphQL 的 media 对象里没有字节数，
/// 于是所有媒体下载都落在"未校验"分支（`docs/02` §E2 说那是最不该发生的）。
#[tokio::test]
async fn unknown_size_is_probed_so_integrity_is_still_verified() {
    let server = FixtureServer::start().await;
    let dir = workdir("probe-auto");
    let queue = DownloadQueue::start(config(&dir, 1))
        .await
        .expect("启动队列");
    let dest = dir.join("auto.bin");
    let size = 6144u64;

    // **刻意不给 expect_size**
    queue
        .enqueue(EnqueueJob::new(
            "job-auto",
            server.url(&format!("/full?size={size}")),
            &dest,
        ))
        .await
        .expect("入队");
    wait_for_state(&queue, "job-auto", &[JobState::Complete]).await;

    assert_eq!(std::fs::read(&dest).unwrap(), body_bytes(size as usize));

    // 证据 1：探测请求真的发了（HEAD），而且**没有**把文件传下来
    let requests = server.requests();
    assert!(
        requests.iter().any(|r| r.method == "HEAD"),
        "应当先发一次 HEAD 问大小：{requests:?}"
    );

    // 证据 2：任务状态里的 total 被填上了（来自探测），而不是一直为 0
    let snapshot = queue.status("job-auto").unwrap();
    assert_eq!(snapshot.total, size, "探测到的大小应当写进任务状态");
    assert_eq!(snapshot.done, size);

    queue.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}
