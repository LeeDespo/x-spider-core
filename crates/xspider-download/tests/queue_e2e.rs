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

#[tokio::test]
async fn rapid_pause_then_resume_serializes_the_old_backend_run() {
    let server = FixtureServer::start().await;
    let dir = workdir("rapid-pause-resume");
    let queue = DownloadQueue::start(config(&dir, 2))
        .await
        .expect("启动队列");
    let size = 96 * 1024u64;
    let dest = dir.join("rapid.bin");
    queue
        .enqueue(
            EnqueueJob::new(
                "job-rapid-resume",
                server.url(&format!("/slow?size={size}")),
                &dest,
            )
            .with_expect_size(size),
        )
        .await
        .expect("入队");
    for _ in 0..100 {
        if std::fs::metadata(xspider_download::part_path(&dest))
            .map(|metadata| metadata.len() > 0)
            .unwrap_or(false)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        std::fs::metadata(xspider_download::part_path(&dest))
            .map(|metadata| metadata.len() > 0)
            .unwrap_or(false),
        "暂停前应有真实 HTTP 字节"
    );

    // 不等待旧请求完成暂停收尾；恢复的新轮必须等它释放文件 ownership。
    queue.pause("job-rapid-resume").expect("暂停");
    queue.resume("job-rapid-resume").expect("立刻恢复");
    wait_for_state(&queue, "job-rapid-resume", &[JobState::Complete]).await;
    assert_eq!(std::fs::read(&dest).unwrap(), body_bytes(size as usize));
    assert!(
        server.requests().iter().any(|request| {
            request
                .range
                .as_deref()
                .is_some_and(|range| range.starts_with("bytes="))
        }),
        "新一轮应续传旧轮留下的实际临时文件"
    );
    queue.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn cancel_after_pause_waits_for_cleanup_before_explicit_resume() {
    let server = FixtureServer::start().await;
    let dir = workdir("pause-cancel-resume");
    let queue = DownloadQueue::start(config(&dir, 1))
        .await
        .expect("启动队列");
    let size = 64 * 1024u64;
    let dest = dir.join("cancelled.bin");
    queue
        .enqueue(
            EnqueueJob::new(
                "job-pause-cancel",
                server.url(&format!("/slow?size={size}")),
                &dest,
            )
            .with_expect_size(size),
        )
        .await
        .expect("入队");
    for _ in 0..100 {
        if std::fs::metadata(xspider_download::part_path(&dest))
            .map(|metadata| metadata.len() > 0)
            .unwrap_or(false)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    queue.pause("job-pause-cancel").expect("暂停");
    queue.cancel("job-pause-cancel").expect("暂停后取消");
    wait_for_state(&queue, "job-pause-cancel", &[JobState::Error]).await;
    assert!(!xspider_download::part_path(&dest).exists());
    assert_eq!(
        queue.status("job-pause-cancel").unwrap().reason.as_deref(),
        Some("cancelled")
    );
    queue
        .resume("job-pause-cancel")
        .expect("清理完成的取消任务可以显式重试");
    wait_for_state(&queue, "job-pause-cancel", &[JobState::Complete]).await;
    assert_eq!(std::fs::read(&dest).unwrap(), body_bytes(size as usize));
    let resumed = server.requests();
    assert!(
        resumed
            .last()
            .is_some_and(|request| request.range.is_none()),
        "取消清理后显式重试必须从头开始：{resumed:?}"
    );
    queue.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn cancel_cleanup_failure_keeps_intent_pending_until_a_retry_succeeds() {
    let server = FixtureServer::start().await;
    let dir = workdir("cancel-cleanup-failure");
    let queue = DownloadQueue::start(config(&dir, 1))
        .await
        .expect("启动队列");
    let mut events = queue.subscribe();
    let dest = dir.join("cleanup-failure.bin");
    queue
        .enqueue(
            EnqueueJob::new(
                "job-cancel-cleanup-failure",
                server.url("/slow?size=65536"),
                &dest,
            )
            .with_expect_size(65536),
        )
        .await
        .expect("入队");
    for _ in 0..100 {
        if std::fs::metadata(xspider_download::part_path(&dest))
            .map(|metadata| metadata.len() > 0)
            .unwrap_or(false)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    wait_for_state(&queue, "job-cancel-cleanup-failure", &[JobState::Active]).await;
    let part = xspider_download::part_path(&dest);
    let control = part.with_file_name(format!(
        "{}.aria2",
        part.file_name().unwrap().to_string_lossy()
    ));
    std::fs::create_dir(&control).expect("制造不能按文件删除的控制路径");

    // Keep it Active while its backend is running. This reproduces a cleanup error
    // after the runner exits: a later cancel must not mistake the stale Active
    // snapshot for a live runner and leave the task stuck forever.
    queue
        .cancel("job-cancel-cleanup-failure")
        .expect("请求取消");
    let cleanup_failure = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(DownloadEvent::Failed { job_id, reason, .. }) = events.recv().await {
                if job_id == "job-cancel-cleanup-failure" && reason == "cancel_cleanup_failed" {
                    break;
                }
            }
        }
    })
    .await;
    assert!(cleanup_failure.is_ok(), "应收到断点清理失败事件");
    assert_eq!(
        queue.status("job-cancel-cleanup-failure").unwrap().state,
        JobState::Active,
        "清理失败时不能宣告取消终态"
    );
    let records: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("records.json")).expect("读取消 intent"))
            .unwrap();
    assert_eq!(
        records["jobs"]["job-cancel-cleanup-failure"]["cancel_requested"], true,
        "清理失败必须保留 durable intent"
    );

    std::fs::remove_dir(&control).expect("修复控制路径");
    queue
        .cancel("job-cancel-cleanup-failure")
        .expect("再次取消应重试清理");
    wait_for_state(&queue, "job-cancel-cleanup-failure", &[JobState::Error]).await;
    assert!(!part.exists());
    assert!(!control.exists());
    assert_eq!(
        queue
            .status("job-cancel-cleanup-failure")
            .unwrap()
            .reason
            .as_deref(),
        Some("cancelled")
    );
    queue.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// Paused survives process restart as Paused; an explicit resume uses the actual part length.
#[tokio::test]
async fn paused_job_survives_restart_and_resumes_with_saved_parameters() {
    let server = FixtureServer::start().await;
    let dir = workdir("paused-restart");
    let size = 64 * 1024u64;
    let dest = dir.join("paused.bin");
    let mut job = EnqueueJob::new(
        "job-paused-restart",
        server.url(&format!("/slow?size={size}")),
        &dest,
    )
    .with_expect_size(size);
    job.requirements = Requirements {
        resume: false,
        segments: 7,
    };
    job.skip_if_present = true;
    job.tag = Some("saved-tag".to_string());

    let queue = DownloadQueue::start(config(&dir, 1))
        .await
        .expect("启动队列");
    queue.enqueue(job.clone()).await.expect("入队");
    for _ in 0..100 {
        if std::fs::metadata(xspider_download::part_path(&dest))
            .map(|m| m.len() >= 2 * 1024)
            .unwrap_or(false)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    queue.pause("job-paused-restart").expect("暂停");
    wait_for_state(&queue, "job-paused-restart", &[JobState::Paused]).await;
    let part = xspider_download::part_path(&dest);
    let paused_bytes = std::fs::metadata(&part).unwrap().len();
    assert!(paused_bytes > 0 && paused_bytes < size);

    let records: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("records.json")).expect("读任务记录"))
            .unwrap();
    let saved = &records["jobs"]["job-paused-restart"];
    assert_eq!(saved["state"], "paused");
    assert_eq!(saved["url"], job.url);
    assert_eq!(saved["expect_size"], size);
    assert_eq!(saved["requirements"]["resume"], false);
    assert_eq!(saved["requirements"]["segments"], 7);
    assert_eq!(saved["skip_if_present"], true);
    assert_eq!(saved["tag"], "saved-tag");
    queue.shutdown().await;

    let requests_at_restart = server.requests().len();
    let queue = DownloadQueue::start(config(&dir, 1))
        .await
        .expect("重启队列");
    let restored = queue.status("job-paused-restart").expect("恢复暂停快照");
    assert_eq!(restored.state, JobState::Paused, "暂停任务重启后仍应暂停");
    assert_eq!(restored.done, paused_bytes, "进度以磁盘临时文件为准");
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        server.requests().len(),
        requests_at_restart,
        "Paused 不应在启动时自动请求网络"
    );
    assert_eq!(
        queue.enqueue(job).await.unwrap(),
        AcceptedBy::AlreadyKnown,
        "持久任务重放必须保持幂等"
    );

    queue.resume("job-paused-restart").expect("显式恢复");
    wait_for_state(&queue, "job-paused-restart", &[JobState::Complete]).await;
    assert_eq!(std::fs::read(&dest).unwrap(), body_bytes(size as usize));
    let requests = server.requests();
    assert!(
        requests
            .iter()
            .any(|request| { request.range.as_deref() == Some(&format!("bytes={paused_bytes}-")) }),
        "重启后应从实际断点续传：{requests:?}"
    );
    queue.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn resume_on_an_active_job_is_a_successful_noop() {
    let server = FixtureServer::start().await;
    let dir = workdir("resume-active-noop");
    let queue = DownloadQueue::start(config(&dir, 1))
        .await
        .expect("启动队列");
    let size = 20 * 1024u64;
    queue
        .enqueue(
            EnqueueJob::new(
                "job-active-noop",
                server.url(&format!("/slow?size={size}")),
                dir.join("active.bin"),
            )
            .with_expect_size(size),
        )
        .await
        .expect("入队");
    wait_for_state(&queue, "job-active-noop", &[JobState::Active]).await;
    queue
        .resume("job-active-noop")
        .expect("active resume 应成功");
    wait_for_state(&queue, "job-active-noop", &[JobState::Complete]).await;
    let gets = server
        .requests()
        .into_iter()
        .filter(|request| request.method == "GET")
        .count();
    assert_eq!(gets, 1, "active resume no-op 不能重复派发");
    queue.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// A crash after Active reached disk recovers automatically and bases Range on the part file.
#[tokio::test]
async fn interrupted_active_record_recovers_from_the_disk_part() {
    let server = FixtureServer::start().await;
    let dir = workdir("active-restart");
    let size = 64 * 1024u64;
    let dest = dir.join("active.bin");
    let job_id = "job-active-restart";

    let queue = DownloadQueue::start(config(&dir, 1))
        .await
        .expect("启动队列");
    queue
        .enqueue(
            EnqueueJob::new(job_id, server.url(&format!("/slow?size={size}")), &dest)
                .with_expect_size(size),
        )
        .await
        .expect("入队");
    for _ in 0..100 {
        if std::fs::metadata(xspider_download::part_path(&dest))
            .map(|m| m.len() >= 2 * 1024)
            .unwrap_or(false)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    queue.pause(job_id).expect("停住以模拟进程中断");
    wait_for_state(&queue, job_id, &[JobState::Paused]).await;
    let paused_bytes = std::fs::metadata(xspider_download::part_path(&dest))
        .unwrap()
        .len();
    queue.shutdown().await;

    // 暂停记录和临时文件都来自真实下载；将持久状态改为 Active 模拟未能优雅收尾的进程中断。
    let records_path = dir.join("records.json");
    let mut records: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&records_path).unwrap()).unwrap();
    records["jobs"][job_id]["state"] = serde_json::json!("active");
    std::fs::write(&records_path, serde_json::to_vec_pretty(&records).unwrap()).unwrap();

    let requests_before_recovery = server.requests().len();
    let queue = DownloadQueue::start(config(&dir, 1))
        .await
        .expect("恢复 Active");
    wait_for_state(&queue, job_id, &[JobState::Complete]).await;
    assert_eq!(std::fs::read(&dest).unwrap(), body_bytes(size as usize));
    let requests = &server.requests()[requests_before_recovery..];
    assert!(
        requests
            .iter()
            .any(|request| { request.range.as_deref() == Some(&format!("bytes={paused_bytes}-")) }),
        "Active 重启恢复应从临时文件长度续传：{requests:?}"
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
    let requests_before_restart = server.requests().len();
    let queue = DownloadQueue::start(config(&dir, 1))
        .await
        .expect("取消后重启");
    let restored = queue.status("job-c").expect("取消记录应恢复为终态快照");
    assert_eq!(restored.state, JobState::Error);
    assert_eq!(restored.reason.as_deref(), Some("cancelled"));
    assert_eq!(
        queue
            .enqueue(EnqueueJob::new(
                "job-c",
                server.url("/slow?size=20480"),
                &dest,
            ))
            .await
            .unwrap(),
        AcceptedBy::AlreadyKnown,
        "取消记录仍阻止重复 enqueue"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        server.requests().len(),
        requests_before_restart,
        "取消不能在重启时自动复活"
    );

    // 明确的 resume 是调用方要求重试，取消已清理断点，所以必须从头开始。
    queue.resume("job-c").expect("允许显式重试已取消任务");
    wait_for_state(&queue, "job-c", &[JobState::Complete]).await;
    assert_eq!(std::fs::read(&dest).unwrap(), body_bytes(20_480));
    let retry_requests = &server.requests()[requests_before_restart..];
    assert!(
        retry_requests
            .iter()
            .any(|request| request.method == "GET" && request.range.is_none()),
        "显式 retry 应从 0 开始，而不是续接取消前断点：{retry_requests:?}"
    );
    queue.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn pending_cancel_recovery_cleans_part_but_preserves_ambiguous_final_file() {
    let server = FixtureServer::start().await;
    let dir = workdir("pending-cancel-restart");
    let dest = dir.join("ambiguous.bin");
    let original = b"possibly another task's completed file";
    std::fs::write(&dest, original).unwrap();
    let part = xspider_download::part_path(&dest);
    std::fs::write(&part, b"partial bytes").unwrap();

    // Simulate a crash after cancel intent was durable but before cleanup/terminal
    // persistence. A final path cannot be proven to belong to this job after a
    // restart, even if the old record says it began absent.
    let record = xspider_download::RecordEntry {
        state: JobState::Active,
        dest_path: dest.display().to_string(),
        bytes: 13,
        url: server.url("/full?size=13"),
        expect_size: Some(13),
        completed_at: None,
        tag: None,
        requirements: Requirements::default(),
        skip_if_present: false,
        reason: None,
        error: None,
        cancel_requested: true,
        destination_preexisting: Some(false),
    };
    let file = xspider_download::RecordsFile {
        version: 1,
        jobs: [("pending-cancel".to_string(), record)]
            .into_iter()
            .collect(),
    };
    std::fs::write(
        dir.join("records.json"),
        serde_json::to_vec_pretty(&file).unwrap(),
    )
    .unwrap();

    let queue = DownloadQueue::start(config(&dir, 1))
        .await
        .expect("恢复 pending cancel");
    let restored = queue.status("pending-cancel").expect("恢复取消快照");
    assert_eq!(restored.state, JobState::Error);
    assert_eq!(restored.reason.as_deref(), Some("cancelled"));
    assert_eq!(std::fs::read(&dest).unwrap(), original);
    assert!(!part.exists(), "可确认属于临时断点的文件应清理");
    assert!(server.requests().is_empty(), "pending cancel 不能自动复活");
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

/// Version 1 records written before the new optional fields still load with their defaults.
#[tokio::test]
async fn legacy_version_one_record_recovers_with_default_requirements() {
    let server = FixtureServer::start().await;
    let dir = workdir("legacy-record");
    let size = 4096u64;
    let dest = dir.join("legacy.bin");
    let part = xspider_download::part_path(&dest);
    std::fs::write(&part, body_bytes(1024)).unwrap();
    let records = serde_json::json!({
        "version": 1,
        "jobs": {
            "job-legacy": {
                "state": "active",
                "dest_path": dest.display().to_string(),
                "bytes": 12,
                "url": server.url(&format!("/full?size={size}")),
                "expect_size": size,
                "completed_at": null,
                "tag": "old-tag"
            }
        }
    });
    std::fs::write(
        dir.join("records.json"),
        serde_json::to_vec_pretty(&records).unwrap(),
    )
    .unwrap();

    let queue = DownloadQueue::start(config(&dir, 1))
        .await
        .expect("兼容旧格式");
    wait_for_state(&queue, "job-legacy", &[JobState::Complete]).await;
    assert_eq!(std::fs::read(&dest).unwrap(), body_bytes(size as usize));
    let requests = server.requests();
    assert!(
        requests
            .iter()
            .any(|request| request.range.as_deref() == Some("bytes=1024-")),
        "实际临时文件长度应覆盖旧记录里的陈旧 bytes：{requests:?}"
    );
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("records.json")).unwrap()).unwrap();
    assert_eq!(saved["jobs"]["job-legacy"]["requirements"]["resume"], true);
    assert_eq!(saved["jobs"]["job-legacy"]["requirements"]["segments"], 1);
    assert_eq!(saved["jobs"]["job-legacy"]["skip_if_present"], false);
    assert_eq!(saved["jobs"]["job-legacy"]["tag"], "old-tag");
    queue.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// A failed initial record write is returned to the caller and the task is never dispatched.
#[tokio::test]
async fn enqueue_does_not_accept_or_dispatch_when_record_write_fails() {
    let server = FixtureServer::start().await;
    let dir = workdir("record-write-failure");
    let records_path = dir.join("records.json");
    std::fs::create_dir(&records_path).unwrap();
    let mut cfg = config(&dir, 1);
    cfg.records_path = Some(records_path);
    let queue = DownloadQueue::start(cfg).await.expect("启动队列");

    let result = queue
        .enqueue(
            EnqueueJob::new(
                "job-no-record",
                server.url("/full?size=1024"),
                dir.join("x.bin"),
            )
            .with_expect_size(1024),
        )
        .await;
    assert!(result.is_err(), "入队必须暴露记录写入失败");
    assert!(
        queue.status("job-no-record").is_none(),
        "失败任务不能进入队列"
    );
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(server.requests().is_empty(), "写盘失败后不应发下载请求");

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
    let requests_before_restart = server.requests().len();
    let queue = DownloadQueue::start(config(&dir, 1))
        .await
        .expect("错误任务重启");
    let restored = queue.status("job-i").expect("错误快照应恢复");
    assert_eq!(restored.state, JobState::Error);
    assert_eq!(restored.reason.as_deref(), Some("integrity_failed"));
    assert_eq!(
        server.requests().len(),
        requests_before_restart,
        "Error 不自动重试"
    );
    assert_eq!(
        queue
            .enqueue(EnqueueJob::new(
                "job-i",
                server.url("/full?size=1024"),
                &dest,
            ))
            .await
            .unwrap(),
        AcceptedBy::AlreadyKnown,
        "错误记录继续参与幂等"
    );
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
    let requests_before_restart = server.requests().len();
    let queue = DownloadQueue::start(config(&dir, 2))
        .await
        .expect("修剪后重启");
    assert!(queue.list().is_empty(), "完成记录不重建内存快照");
    assert_eq!(queue.record_count(), 3, "prune 保留完成记录以供幂等");
    assert_eq!(
        queue
            .enqueue(EnqueueJob::new(
                "p0",
                server.url("/full?size=512"),
                dir.join("p0.bin"),
            ))
            .await
            .unwrap(),
        AcceptedBy::AlreadyKnown
    );
    assert_eq!(server.requests().len(), requests_before_restart);
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
