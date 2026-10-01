//! **Aria2Next 外派后端 E2E**：真二进制 + 本地 HTTP fixture server（全程离线）。
//!
//! 这几条测试的存在理由很具体：**Aria2Next 有两个行为会骗人**，而它们只有真跑才看得到——
//!
//! 1. **404 会被报成"成功"**（实测：`status=complete, errorCode=0`，还留下 0 字节文件）；
//! 2. **RPC 层的错误全是 `code: 1`**（`Unknown option` / `GID not found` / `Unauthorized` 一样），
//!    所以"按码分类"在 RPC 这一层根本不可用。
//!
//! 没有真二进制就**大声跳过**（不是静默通过）：
//! 用 `XSPIDER_ARIA2_PATH` 指定，或让它落在 [`Aria2NextConfig::locate_binary`] 能找到的位置。

mod common;

use std::path::{Path, PathBuf};

use common::{body_bytes, FixtureServer};
use xspider_core::cancel::CancelToken;
use xspider_download::{
    part_path_for, Aria2Next, Aria2NextConfig, DownloadError, DownloadRequest, Integrity,
    ATTRIBUTION_ARIA2, ATTRIBUTION_HTTP,
};

/// 找一个二进制；找不到就打印一行**醒目**的跳过说明并返回 None。
fn binary() -> Option<PathBuf> {
    match Aria2NextConfig::locate_binary() {
        Some(path) => Some(path),
        None => {
            eprintln!(
                "！！跳过 Aria2Next E2E：没找到二进制。\
                 用 XSPIDER_ARIA2_PATH=/path/to/aria2next 指定；\
                 这几条测试覆盖的是「引擎报成功但其实失败」这类骗人行为，不要长期不跑。"
            );
            None
        }
    }
}

fn workdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "xspider-a2e2e-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("创建临时目录失败");
    dir
}

async fn engine(binary: PathBuf, dir: &Path) -> Aria2Next {
    Aria2Next::spawn(Aria2NextConfig::new(binary, dir))
        .await
        .expect("启动 Aria2Next 失败")
}

#[tokio::test]
async fn aria2next_downloads_a_file_and_verifies_integrity() {
    let Some(binary) = binary() else { return };
    let server = FixtureServer::start().await;
    let dir = workdir("normal");
    let dest = dir.join("pic.bin");
    let size = 64 * 1024usize;

    let mut engine = engine(binary, &dir).await;
    // 版本来自引擎自己报告的 product/version，不是猜的
    assert!(
        engine.version().starts_with('2') || engine.version().starts_with("2."),
        "版本号看起来不对：{}",
        engine.version()
    );

    let outcome = engine
        .download(
            &DownloadRequest::new(server.url(&format!("/full?size={size}")), &dest)
                .with_expect_size(size as u64),
            &CancelToken::new(),
        )
        .await
        .expect("Aria2Next 下载应当成功");

    assert_eq!(outcome.bytes, size as u64);
    assert_eq!(
        outcome.integrity,
        Integrity::Verified {
            expected: size as u64,
            actual: size as u64
        }
    );
    assert_eq!(
        std::fs::read(&dest).expect("文件应当已落盘"),
        body_bytes(size),
        "内容必须逐字节一致"
    );
    // 引擎的临时文件与控制文件都不许留下（docs/02 §E4）
    assert!(
        !part_path_for(&dest, ATTRIBUTION_ARIA2).exists(),
        "不该留下 .part.aria2next"
    );
    let control = dir.join(format!(
        "{}.aria2",
        part_path_for(&dest, ATTRIBUTION_ARIA2)
            .file_name()
            .unwrap()
            .to_string_lossy()
    ));
    assert!(!control.exists(), "不该留下 .aria2 控制文件");

    engine.shutdown().await.expect("优雅关停失败");
    let _ = std::fs::remove_dir_all(&dir);
}

/// **这条是本文件最重要的测试**：Aria2Next 对 404 会报 `status=complete`。
/// 如果照着"引擎说完成就算完成"写，这里会静默产出一个 0 字节文件并汇报成功。
#[tokio::test]
async fn a_404_must_not_be_reported_as_success() {
    let Some(binary) = binary() else { return };
    let server = FixtureServer::start().await;
    let dir = workdir("404");
    let dest = dir.join("nope.bin");

    let engine = engine(binary, &dir).await;
    let result = engine
        .download(
            &DownloadRequest::new(server.url("/missing"), &dest).with_expect_size(100),
            &CancelToken::new(),
        )
        .await;

    match result {
        Err(DownloadError::NotFound) => {}
        // 实测路径：引擎报 complete + 0 字节 → 我们的校验必须拦住它
        Err(DownloadError::IntegrityFailed { actual: 0, .. }) => {}
        Ok(outcome) => panic!(
            "404 被汇报成成功了（bytes={}）——这正是 Aria2Next 会骗人的地方",
            outcome.bytes
        ),
        Err(other) => panic!("期望 NotFound 或 IntegrityFailed，实际 {other:?}"),
    }
    assert!(!dest.exists(), "失败的下载不许在目标路径留下文件");
    assert!(
        !part_path_for(&dest, ATTRIBUTION_ARIA2).exists(),
        "失败后引擎的半成品必须被清掉"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn size_mismatch_is_integrity_failed_and_cleaned_up() {
    let Some(binary) = binary() else { return };
    let server = FixtureServer::start().await;
    let dir = workdir("integrity");
    let dest = dir.join("pic.bin");

    let engine = engine(binary, &dir).await;
    let err = engine
        .download(
            // 服务端只有 8KB，但我们期望 999KB
            &DownloadRequest::new(server.url("/full?size=8192"), &dest).with_expect_size(999_000),
            &CancelToken::new(),
        )
        .await
        .expect_err("大小不符必须失败");

    match err {
        DownloadError::IntegrityFailed { expected, actual } => {
            assert_eq!(expected, 999_000);
            assert_eq!(actual, 8192);
        }
        other => panic!("期望 IntegrityFailed，实际 {other:?}"),
    }
    assert!(!dest.exists());
    assert!(!part_path_for(&dest, ATTRIBUTION_ARIA2).exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn cancel_stops_the_task_and_leaves_nothing_behind() {
    let Some(binary) = binary() else { return };
    let server = FixtureServer::start().await;
    let dir = workdir("cancel");
    let dest = dir.join("pic.bin");

    let engine = engine(binary, &dir).await;
    let cancel = CancelToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        trigger.cancel();
    });

    let err = engine
        .download(
            // 慢速流：64KB、每 50ms 1KB → 取消必然发生在传输中途
            &DownloadRequest::new(server.url("/slow?size=65536"), &dest),
            &cancel,
        )
        .await
        .expect_err("取消必须失败");

    assert_eq!(err, DownloadError::Cancelled);
    assert!(!dest.exists(), "取消后目标路径必须干净");
    assert!(
        !part_path_for(&dest, ATTRIBUTION_ARIA2).exists(),
        "取消后引擎的半成品必须被清掉"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 换引擎**绝不复用别人的断点**（`docs/02` §E3：拼在一起会产出损坏文件）。
#[tokio::test]
async fn a_partial_from_another_engine_is_not_reused() {
    let Some(binary) = binary() else { return };
    let server = FixtureServer::start().await;
    let dir = workdir("cross-engine");
    let dest = dir.join("pic.bin");
    let size = 8192usize;

    // 伪造一个"内置 HTTP 后端留下的"断点文件（垃圾内容）
    let http_part = part_path_for(&dest, ATTRIBUTION_HTTP);
    std::fs::write(&http_part, vec![0xAAu8; 1024]).expect("写假断点");

    let engine = engine(binary, &dir).await;
    let outcome = engine
        .download(
            &DownloadRequest::new(server.url(&format!("/full?size={size}")), &dest)
                .with_expect_size(size as u64),
            &CancelToken::new(),
        )
        .await
        .expect("应当正常下完");

    assert_eq!(outcome.bytes, size as u64);
    assert_eq!(
        std::fs::read(&dest).unwrap(),
        body_bytes(size),
        "必须与完整内容一致——把别的引擎的断点拼进来就会错"
    );
    assert!(
        http_part.exists(),
        "别的引擎的断点不归我们管，不该动它（清理是那个引擎的事）"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 只接受 Aria2Next：指向上游 aria2 必须在**预检**就被拒（不必等 RPC）。
#[tokio::test]
async fn plain_aria2_is_refused() {
    // Homebrew 的 aria2c 是最方便的真实反例
    let candidates = ["/opt/homebrew/bin/aria2c", "/usr/local/bin/aria2c"];
    let Some(binary) = candidates.iter().map(PathBuf::from).find(|p| p.is_file()) else {
        eprintln!("旁路：本机没有上游 aria2c，跳过「拒绝上游 aria2」这条用例");
        return;
    };

    let err = Aria2Next::spawn(Aria2NextConfig::new(binary, std::env::temp_dir()))
        .await
        .expect_err("指向上游 aria2 必须被拒");
    match err {
        DownloadError::Invalid { message } => {
            assert!(message.contains("Aria2Next"), "{message}");
        }
        other => panic!("期望 Invalid，实际 {other:?}"),
    }
}

/// 关停之后**不留子进程**（`docs/05` §6 明确要求冒烟与测试都断言这一条）。
#[tokio::test]
async fn shutdown_leaves_no_child_process() {
    let Some(binary) = binary() else { return };
    let dir = workdir("shutdown");
    let mut engine = engine(binary.clone(), &dir).await;

    // 拿不到 pid 时至少验证"shutdown 能正常返回且之后 RPC 不再响应"
    let port = engine.port();
    engine.shutdown().await.expect("优雅关停失败");

    // 端口必须真的释放了（RPC 连不上 = 进程退了）
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(500))
        .no_proxy()
        .build()
        .unwrap();
    let still_alive = client
        .post(format!("http://127.0.0.1:{port}/jsonrpc"))
        .json(
            &serde_json::json!({"jsonrpc":"2.0","id":"1","method":"aria2.getVersion","params":[]}),
        )
        .send()
        .await
        .is_ok();
    assert!(!still_alive, "关停之后 RPC 端口不该还在响应");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Drop 也要能收尾（不允许"忘了调 shutdown 就留一堆 aria2 进程"）。
#[tokio::test]
async fn dropping_the_engine_kills_the_child_process() {
    let Some(binary) = binary() else { return };
    let dir = workdir("drop");
    let engine = engine(binary, &dir).await;
    let port = engine.port();
    drop(engine);

    // 给内核一点时间回收
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(500))
        .no_proxy()
        .build()
        .unwrap();
    let still_alive = client
        .post(format!("http://127.0.0.1:{port}/jsonrpc"))
        .json(
            &serde_json::json!({"jsonrpc":"2.0","id":"1","method":"aria2.getVersion","params":[]}),
        )
        .send()
        .await
        .is_ok();
    assert!(!still_alive, "Drop 之后子进程必须已经死了");
    let _ = std::fs::remove_dir_all(&dir);
}
