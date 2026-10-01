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
    if let Some(dir) = &args.fixture_dir {
        tracing::warn!(
            dir = %dir.display(),
            "fixture 回放模式：不会发出任何真实网络请求（测试专用）"
        );
        return Engine::replay(dir)
            .map(|engine| engine.with_transport(Transport::Sidecar))
            .map_err(|e| format!("加载 fixture 失败：{e}"));
    }
    xspider::engine_from_env()
        .map(|engine| engine.with_transport(Transport::Sidecar))
        .map_err(|e| format!("构造引擎失败：{e}"))
}

async fn serve(args: Args, engine: Engine) -> Result<(), String> {
    let shutdown = Arc::new(Notify::new());
    spawn_signal_handlers(shutdown.clone());

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
