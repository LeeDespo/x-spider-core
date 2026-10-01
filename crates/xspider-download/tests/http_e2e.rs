//! **下载 E2E（离线）**：本地 HTTP fixture server + 内置 HTTP 后端。
//!
//! 对应 `docs/04-TESTING-AND-FIXTURES.md` §5 的场景表：
//! 正常 200 / Range 206 / 中途断流 / 404 / 大小不符 / 慢速+取消。
//! 全程**本地回环**，`cargo test` 不碰真网络（铁律 3）。
//!
//! 每条测试都断言"**目标路径的最终状态**"——因为下载最典型的失败不是"报错"，
//! 而是**悄悄留下一个半截文件**（`docs/02` §E2：不可达 URL 的实测表现是留下 0 字节文件）。

mod common;

use std::path::PathBuf;

use common::{body_bytes, FixtureServer};
use xspider_core::cancel::CancelToken;
use xspider_core::http::ProxyConfig;
use xspider_core::ratelimit::Limits;
use xspider_download::{part_path, DownloadError, DownloadRequest, HttpDownloader, Integrity};

fn downloader() -> HttpDownloader {
    // Off：本地回环绝不能走代理（否则测试会被环境里的 HTTPS_PROXY 劫持）
    HttpDownloader::new(&ProxyConfig::Off, Limits::default()).expect("构造下载器失败")
}

/// 每个测试用独立目录，避免相互污染。
fn workdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "xspider-dl-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("创建临时目录失败");
    dir
}

#[tokio::test]
async fn normal_download_lands_exact_bytes_and_verifies_size() {
    let server = FixtureServer::start().await;
    let dir = workdir("normal");
    let dest = dir.join("pic.bin");
    let size = 5120usize;

    let outcome = downloader()
        .download(
            &DownloadRequest::new(server.url(&format!("/full?size={size}")), &dest)
                .with_expect_size(size as u64),
            &CancelToken::new(),
        )
        .await
        .expect("正常下载应当成功");

    assert_eq!(outcome.bytes, size as u64);
    assert_eq!(outcome.resumed_from, 0, "全新下载不该有续传起点");
    assert_eq!(
        outcome.integrity,
        Integrity::Verified {
            expected: size as u64,
            actual: size as u64
        }
    );
    let actual = std::fs::read(&dest).expect("文件应当已落盘");
    assert_eq!(actual, body_bytes(size), "内容必须逐字节一致");
    assert!(!part_path(&dest).exists(), "成功之后不该留下 .part");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `expect_size` 与实际不符 → **`integrity_failed`，不是 `completed`**
/// （`docs/02` §E2：完整性校验是唯一的成功判据）。
#[tokio::test]
async fn size_mismatch_is_integrity_failed_and_leaves_nothing_behind() {
    let server = FixtureServer::start().await;
    let dir = workdir("integrity");
    let dest = dir.join("pic.bin");

    let err = downloader()
        .download(
            // 服务端只有 100 字节，但我们期望 999
            &DownloadRequest::new(server.url("/full?size=100"), &dest).with_expect_size(999),
            &CancelToken::new(),
        )
        .await
        .expect_err("大小不符必须失败");

    match err {
        DownloadError::IntegrityFailed { expected, actual } => {
            assert_eq!(expected, 999);
            assert_eq!(actual, 100);
        }
        other => panic!("期望 IntegrityFailed，实际 {other:?}"),
    }
    assert!(!dest.exists(), "校验失败不该在目标路径留下文件");
    assert!(!part_path(&dest).exists(), "校验失败应当清掉 .part");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 中途断流 → 重试并且**从已落盘字节数续传**（不是从头再来）。
#[tokio::test]
async fn disconnect_mid_stream_is_retried_and_resumed_from_the_written_bytes() {
    let server = FixtureServer::start().await;
    let dir = workdir("resume");
    let dest = dir.join("pic.bin");
    let size = 8192usize;
    let cut = 2048usize;

    let outcome = downloader()
        .download(
            &DownloadRequest::new(
                server.url(&format!("/cut_once?size={size}&cut={cut}")),
                &dest,
            )
            .with_expect_size(size as u64),
            &CancelToken::new(),
        )
        .await
        .expect("断流之后应当重试成功");

    assert_eq!(outcome.bytes, size as u64);
    assert_eq!(
        outcome.attempts, 2,
        "应当是「第一次截断 + 第二次续传成功」两次尝试"
    );
    assert_eq!(
        std::fs::read(&dest).unwrap(),
        body_bytes(size),
        "续传拼出来的内容必须逐字节正确（拼接错位会在这里红）"
    );

    // 关键证据：第二次请求**带了 Range**，而且从已落盘处开始
    let requests = server.requests();
    assert_eq!(requests.len(), 2, "应当正好两次请求：{requests:?}");
    assert_eq!(requests[0].range, None, "第一次是全新下载，不该带 Range");
    assert_eq!(
        requests[1].range.as_deref(),
        Some(format!("bytes={cut}-").as_str()),
        "第二次必须从已落盘字节数续传：{requests:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 服务端**忽略 Range**（返回 200 全量）时必须**从头写**，
/// 否则会把"前 2KB + 完整文件"拼成一个错位的大文件。
#[tokio::test]
async fn a_server_ignoring_range_forces_a_restart_instead_of_corrupting_the_file() {
    let server = FixtureServer::start().await;
    let dir = workdir("ignore-range");
    let dest = dir.join("pic.bin");
    let size = 4096usize;

    // 先造一个半成品断点文件
    let part = part_path(&dest);
    std::fs::write(&part, body_bytes(1024)).expect("写断点文件");

    let outcome = downloader()
        .download(
            &DownloadRequest::new(
                server.url(&format!("/full_ignoring_range?size={size}")),
                &dest,
            )
            .with_expect_size(size as u64),
            &CancelToken::new(),
        )
        .await
        .expect("应当能从头重下并成功");

    assert_eq!(outcome.bytes, size as u64);
    assert_eq!(
        std::fs::read(&dest).unwrap(),
        body_bytes(size),
        "必须与完整内容一致——错位拼接会变成远超 {size} 字节的大文件",
    );
    assert_eq!(
        std::fs::read(&dest).unwrap().len(),
        size,
        "文件长度必须正好是 size（多出断点那 1KB 就说明没有从头写）"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 一直断流 → 重试到上限后失败，且**不留下任何半成品**。
#[tokio::test]
async fn permanent_truncation_fails_after_the_attempt_limit_and_cleans_up() {
    let server = FixtureServer::start().await;
    let dir = workdir("always-cut");
    let dest = dir.join("pic.bin");

    let request = DownloadRequest {
        max_attempts: 3,
        ..DownloadRequest::new(server.url("/always_cut?size=4096&cut=512"), &dest)
            .with_expect_size(4096)
    };
    let err = downloader()
        .download(&request, &CancelToken::new())
        .await
        .expect_err("一直截断必须最终失败");

    assert!(
        matches!(
            err,
            DownloadError::Transport { .. } | DownloadError::Truncated { .. }
        ),
        "期望传输/截断类错误，实际 {err:?}"
    );
    assert_eq!(server.requests().len(), 3, "应当正好用满 3 次尝试");
    assert!(!dest.exists(), "失败后目标路径必须干净");
    assert!(
        !part_path(&dest).exists(),
        "失败后不该留下 .part（docs/02 §E5）"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 404 / 403 **不重试**（重试服务端说的"不行"只会放大问题）。
#[tokio::test]
async fn not_found_and_forbidden_are_not_retried() {
    let server = FixtureServer::start().await;
    let dir = workdir("http-errors");

    let err = downloader()
        .download(
            &DownloadRequest::new(server.url("/missing"), dir.join("a.bin")),
            &CancelToken::new(),
        )
        .await
        .expect_err("404 必须失败");
    assert_eq!(err, DownloadError::NotFound);
    assert_eq!(server.requests().len(), 1, "404 不该重试");

    let err = downloader()
        .download(
            &DownloadRequest::new(server.url("/forbidden"), dir.join("b.bin")),
            &CancelToken::new(),
        )
        .await
        .expect_err("403 必须失败");
    assert_eq!(err, DownloadError::AuthRequired { status: 403 });
    assert_eq!(server.requests().len(), 2, "403 不该重试");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 5xx 也不重试（它是"上游说不行"，不是链路抖动）。
#[tokio::test]
async fn server_error_is_reported_with_its_status() {
    let server = FixtureServer::start().await;
    let dir = workdir("boom");
    let err = downloader()
        .download(
            &DownloadRequest::new(server.url("/boom"), dir.join("a.bin")),
            &CancelToken::new(),
        )
        .await
        .expect_err("500 必须失败");
    assert_eq!(err, DownloadError::Upstream { status: 500 });
    assert_eq!(server.requests().len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 取消：**立刻停**，并且不留临时文件（`docs/04` §5 的"无残留"断言）。
#[tokio::test]
async fn cancel_stops_the_transfer_and_removes_the_partial_file() {
    let server = FixtureServer::start().await;
    let dir = workdir("cancel");
    let dest = dir.join("pic.bin");
    let part = part_path(&dest);

    let cancel = CancelToken::new();
    let trigger = cancel.clone();
    // 慢速流：每 50ms 才 1KB，取消必然发生在传输中途
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        trigger.cancel();
    });

    let err = downloader()
        .download(
            &DownloadRequest::new(server.url("/slow?size=65536"), &dest),
            &cancel,
        )
        .await
        .expect_err("取消必须失败");

    assert_eq!(err, DownloadError::Cancelled);
    assert!(!dest.exists(), "取消后目标路径必须干净");
    assert!(
        !part.exists(),
        "取消要清掉 .part（「放弃」与「暂停」不同：暂停要留断点）"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 未给 `expect_size` 时只记实际字节数，**不能声称"校验通过"**。
#[tokio::test]
async fn without_expect_size_the_result_is_explicitly_unverified() {
    let server = FixtureServer::start().await;
    let dir = workdir("unverified");
    let dest = dir.join("pic.bin");

    let outcome = downloader()
        .download(
            &DownloadRequest::new(server.url("/full?size=777"), &dest),
            &CancelToken::new(),
        )
        .await
        .expect("没有期望大小也应当能下");

    assert_eq!(
        outcome.integrity,
        Integrity::Unverified { actual: 777 },
        "没给期望大小就只能「未校验」——不能伪装成 Verified"
    );
    assert!(dest.exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// 断点在磁盘上存在时，**下一次调用会接着用**（这是"暂停后恢复"的底层能力）。
#[tokio::test]
async fn an_existing_part_file_is_resumed_by_a_later_call() {
    let server = FixtureServer::start().await;
    let dir = workdir("later-resume");
    let dest = dir.join("pic.bin");
    let size = 4096usize;
    let already = 1024usize;

    std::fs::write(part_path(&dest), body_bytes(already)).expect("写断点文件");

    let outcome = downloader()
        .download(
            &DownloadRequest::new(server.url(&format!("/full?size={size}")), &dest)
                .with_expect_size(size as u64),
            &CancelToken::new(),
        )
        .await
        .expect("应当从断点续传并成功");

    assert_eq!(outcome.resumed_from, already as u64);
    assert_eq!(outcome.attempts, 1, "一次就够了——断点是有效起点");
    assert_eq!(std::fs::read(&dest).unwrap(), body_bytes(size));
    let requests = server.requests();
    assert_eq!(
        requests[0].range.as_deref(),
        Some(format!("bytes={already}-").as_str()),
        "第一次请求就该带 Range：{requests:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 声明大小与实际不符（服务端撒谎）→ 必须失败，不能把短文件当成功。
#[tokio::test]
async fn a_lying_content_length_fails_instead_of_succeeding_with_a_short_file() {
    let server = FixtureServer::start().await;
    let dir = workdir("liar");
    let dest = dir.join("pic.bin");

    let err = downloader()
        .download(
            &DownloadRequest::new(server.url("/liar?declared=100&actual=40"), &dest)
                .with_expect_size(100),
            &CancelToken::new(),
        )
        .await
        .expect_err("服务端撒谎必须失败");

    assert!(
        matches!(
            err,
            DownloadError::Transport { .. } | DownloadError::Truncated { .. }
        ),
        "期望传输/截断类错误，实际 {err:?}"
    );
    assert!(!dest.exists(), "目标路径不许留下短文件");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// 大小探测（`probe_size`）：图片/视频的真实大小只有服务端知道
// ---------------------------------------------------------------------------

/// `HEAD` 的 `Content-Length` 是**最省**的那条路（不下载任何字节）。
#[tokio::test]
async fn probe_size_uses_head_content_length() {
    let server = FixtureServer::start().await;
    let downloader = downloader();
    let size = 12_345u64;

    let probed = downloader
        .probe_size(
            &server.url(&format!("/head_only?size={size}")),
            &CancelToken::new(),
        )
        .await
        .expect("探测不该失败");
    assert_eq!(probed, Some(size));

    let requests = server.requests();
    assert_eq!(requests.len(), 1, "HEAD 成功就不该再发第二次请求");
    assert_eq!(requests[0].method, "HEAD");
    assert_eq!(
        server.bytes_sent(),
        0,
        "HEAD 一个字节都不该下载——这是它存在的全部意义"
    );
    let _ = server;
}

/// `HEAD` 不被支持（405）时，退到 **1 字节的 Range** 问总长。
///
/// 这不是猜测：实测 `video.twimg.com` 上 HEAD 会走不通，
/// 而 `Range: bytes=0-0` 的 `Content-Range` 稳定给出精确总长。
#[tokio::test]
async fn probe_size_falls_back_to_a_one_byte_range() {
    let server = FixtureServer::start().await;
    let downloader = downloader();
    let size = 15_187_101u64;

    let probed = downloader
        .probe_size(
            &server.url(&format!("/no_head?size={size}")),
            &CancelToken::new(),
        )
        .await
        .expect("探测不该失败");
    assert_eq!(probed, Some(size));

    let requests = server.requests();
    assert_eq!(requests.len(), 2, "HEAD 405 → 再发一次 Range");
    assert_eq!(requests[0].method, "HEAD");
    assert_eq!(requests[1].method, "GET");
    assert_eq!(
        requests[1].range.as_deref(),
        Some("bytes=0-0"),
        "只问 1 个字节"
    );
    assert!(
        server.bytes_sent() <= 1,
        "退回 Range 这条路也只该下载 1 字节，实际 {}",
        server.bytes_sent()
    );
}

/// 探测失败**不该让调用方跟着失败**——`Ok(None)` 就是"服务端没说"。
#[tokio::test]
async fn probe_size_returns_none_when_the_server_says_nothing() {
    let server = FixtureServer::start().await;
    let downloader = downloader();

    // /missing：HEAD 与 Range 都会被拒
    let probed = downloader
        .probe_size(&server.url("/missing"), &CancelToken::new())
        .await;
    assert!(
        matches!(probed, Ok(None) | Err(DownloadError::NotFound)),
        "探测失败应当是可预期的结果，而不是 panic：{probed:?}"
    );
}

/// 探测出来的大小**可以直接当 `expect_size` 用**：下载完必须校验通过。
///
/// 这条把两件事串起来：`crawl` 给出的 `size_hint` → `dl.enqueue` 的 `expect_size`
/// → 完成判据。整条链路是"服务端说了多大，就必须真下到多大"。
#[tokio::test]
async fn a_probed_size_round_trips_through_expect_size() {
    let server = FixtureServer::start().await;
    let downloader = downloader();
    let dir = workdir("probe-roundtrip");
    let dest = dir.join("pic.bin");
    let size = 8192u64;

    let url = server.url(&format!("/full?size={size}"));
    let probed = downloader
        .probe_size(&url, &CancelToken::new())
        .await
        .expect("探测")
        .expect("服务端应当给出大小");

    let outcome = downloader
        .download(
            &DownloadRequest::new(&url, &dest).with_expect_size(probed),
            &CancelToken::new(),
        )
        .await
        .expect("按探测到的大小下载应当成功");
    assert_eq!(outcome.bytes, size);
    assert_eq!(
        outcome.integrity,
        Integrity::Verified {
            expected: size,
            actual: size
        }
    );
    let _ = std::fs::remove_dir_all(&dir);
}
