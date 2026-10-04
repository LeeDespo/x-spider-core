//! `xspiderd` —— X-Spider 的 sidecar 可执行文件（主形态）。
//!
//! 它把 `xspider-ffi` 的能力包成本地 JSON-RPC 服务：**换组件 = 换一个二进制**，
//! 外壳不必为每种语言生成绑定，也不必在启用 hardened runtime 后去
//! `dlopen` 任何 dylib（那条路实测走不通，见 `docs/03` §1）。
//!
//! 它依赖的是 `xspider-ffi` 的 **rlib**，直接调用同一个 `Engine::call`——
//! 所以"cdylib 与 sidecar 行为一致"是结构性的，不靠人工同步两份实现。

mod args;
mod lock;
mod rpc;
mod server;

use std::sync::Arc;

use tokio::sync::Notify;

use xspider::{Engine, Transport};

use crate::args::{Args, Parsed};

fn main() {
    let parsed = match args::parse(std::env::args().skip(1)) {
        Ok(p) => p,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };

    match parsed {
        Parsed::Help => {
            print!("{}", args::USAGE);
        }
        Parsed::Version => {
            println!(
                "xspiderd {} (契约版本 {})",
                xspider_core::BUILD_VERSION,
                xspider_core::CONTRACT_VERSION
            );
        }
        Parsed::Run(args) => {
            if let Err(message) = run(args) {
                eprintln!("xspiderd 启动失败：{message}");
                std::process::exit(1);
            }
        }
    }
}

fn run(args: Args) -> Result<(), String> {
    // 日志一律走 stderr：stdout 是握手通道（`--port 0` 的 ready 行）或 JSON Lines。
    init_tracing();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("xspiderd")
        .build()
        .map_err(|e| format!("构造 tokio 运行时失败：{e}"))?;

    let mut _lock = None;
    if let Some(state_dir) = &args.state_dir {
        _lock = Some(lock::InstanceLock::acquire(state_dir)?);
        tracing::info!(dir = %state_dir.display(), "已取得实例锁");
    }

    // 引擎：`--fixture-dir` 优先（测试专用），否则看环境变量，最后是真网络。
    let engine = build_engine(&args)?;

    runtime.block_on(async move { serve(args, engine).await })
}

fn build_engine(args: &Args) -> Result<Engine, String> {
    let engine = if let Some(dir) = &args.fixture_dir {
        tracing::warn!(
            dir = %dir.display(),
            "fixture 回放模式：不会发出任何真实网络请求（测试专用）"
        );
        Engine::replay(dir).map_err(|e| format!("加载 fixture 失败：{e}"))?
    } else {
        xspider::engine_from_env().map_err(|e| format!("构造引擎失败：{e}"))?
    };
    // `--state-dir` 不只是实例锁目录：它**同时**决定下载记录路径
    // （`docs/07` §2.1 第 5 条）。显式 flag 优先于 `XSPIDER_STATE_DIR`，
    // 优先级规则在 `Engine::with_state_dir` / `resolve_records_path` 里写死并有测试。
    let engine = engine.with_transport(Transport::Sidecar);
    Ok(match &args.state_dir {
        Some(dir) => engine.with_state_dir(dir.clone()),
        None => engine,
    })
}

async fn serve(args: Args, engine: Engine) -> Result<(), String> {
    let shutdown = Arc::new(Notify::new());
    spawn_signal_handlers(shutdown.clone());
    spawn_parent_watchdog(shutdown.clone());

    // `XSPIDER_COOKIE`：启动即注入凭据（等价于外壳先调一次 `auth.set_cookie`）。
    // 给 CLI / 脚本 / 手工调试用——外壳仍应从 Keychain 读、经契约注入。
    // 与一切凭据同等对待：**只进不出**，不打印、不回显、不落盘。
    match std::env::var("XSPIDER_COOKIE") {
        Ok(cookie) if !cookie.trim().is_empty() => {
            match engine
                .call("auth.set_cookie", &serde_json::json!({ "cookie": cookie }))
                .await
            {
                Ok(_) => tracing::info!("已从 XSPIDER_COOKIE 注入凭据"),
                // 不打内容，只说"没被接受"——早失败好过后面拿必然 403 的请求去排查
                Err(e) => return Err(format!("XSPIDER_COOKIE 无效：{e}")),
            }
        }
        _ => {}
    }

    if args.stdio {
        // stdio 模式下 stdout 只走 JSON Lines，ready 行改到 stderr 做诊断
        eprintln!(
            "ready {}",
            serde_json::json!({
                "transport": "stdio",
                "version": xspider_core::CONTRACT_VERSION,
                "build": xspider_core::BUILD_VERSION,
            })
        );
        return server::serve_stdio(engine, shutdown).await;
    }

    let token = args.token.clone().unwrap_or_else(server::generate_token);
    let (listener, port) = server::bind(&args.host, args.port).await?;

    // 握手：先打印 ready 行（并 flush），再开始服务。
    server::print_ready_line(port, &token)?;
    tracing::info!(%port, host = %args.host, "xspiderd 已就绪");

    server::serve_http(listener, engine, token, shutdown).await?;
    tracing::info!("已优雅退出");
    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let filter = tracing_subscriber::EnvFilter::try_from_env("XSPIDER_LOG")
        .or_else(|_| tracing_subscriber::EnvFilter::try_from_env("RUST_LOG"))
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_target(false),
        )
        .try_init();
}

/// **父进程看门狗**：外壳被强杀（SIGKILL / 崩溃）时，组件不能变成孤儿。
///
/// 为什么必须有：外壳正常退出会调 `system.shutdown`，但**强杀不会**——
/// 而强杀是常态（开发时改代码、调试器停进程、测试宿主被 xcodebuild 收走）。
/// 实测：应用被 SIGKILL 之后，`xspiderd` 仍然活着，下一次启动就会多出一个，
/// 累积起来就是"一堆没人管的组件在后台跑"。
///
/// 原理不需要额外机制：父进程一死，子进程会被 reparent 到 `launchd`(pid 1)，
/// 于是 `parent_id()` 变了。`launchd` 直接拉起的实例（parent == 1）不看门。
///
/// 这是 `docs/05-WORKFLOW.md` §6 "子进程必须跟随父进程退出"那条纪律的组件侧实现，
/// 与"交给 aria2 的 `--stop-with-process`"是两件事：那条管的是组件的子进程。
#[cfg(unix)]
fn spawn_parent_watchdog(shutdown: Arc<Notify>) {
    let original = std::os::unix::process::parent_id();
    if original <= 1 {
        // 由 launchd 直接拉起（不是谁的子进程），没有"父进程"可看
        return;
    }
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let now = std::os::unix::process::parent_id();
            if now != original {
                tracing::warn!(original, now, "父进程已消失，组件自行退出（避免留下孤儿）");
                shutdown.notify_waiters();
                return;
            }
        }
    });
}

#[cfg(not(unix))]
fn spawn_parent_watchdog(_shutdown: Arc<Notify>) {}

fn spawn_signal_handlers(shutdown: Arc<Notify>) {
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sigterm = match signal(SignalKind::terminate()) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("注册 SIGTERM 处理失败：{e}");
                    return;
                }
            };
            let mut sigint = match signal(SignalKind::interrupt()) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("注册 SIGINT 处理失败：{e}");
                    return;
                }
            };
            tokio::select! {
                _ = sigterm.recv() => tracing::info!("收到 SIGTERM，开始优雅退出"),
                _ = sigint.recv() => tracing::info!("收到 SIGINT，开始优雅退出"),
            }
        }
        #[cfg(not(unix))]
        {
            if let Err(e) = tokio::signal::ctrl_c().await {
                tracing::error!("监听 Ctrl-C 失败：{e}");
                return;
            }
            tracing::info!("收到 Ctrl-C，开始优雅退出");
        }
        shutdown.notify_waiters();
    });
}
