//! 两种传输：本地 HTTP JSON-RPC（主）与 stdio JSON Lines（备选）。
//!
//! 主形态选 HTTP 的理由（`docs/03-FFI-SIGNING-PACKAGING.md` §3）：
//! 与 aria2 一致、可以用 `curl` 调试、端口随机 + token 鉴权后足够安全。
//!
//! **stdout 的纪律**：`--port 0` 模式下 stdout 只用于打印 ready 行；
//! 日志全部走 stderr。外壳靠读第一行完成握手，混进日志就会握手失败。
//! `--stdio` 模式下 stdout 只走 JSON Lines。

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

use xspider::Engine;
use xspider_core::cancel::CancelToken;
use xspider_core::error::XError;

use crate::rpc;

/// HTTP 状态码的约定（**契约错误一律 200**）：
///
/// | 状态 | 含义 |
/// |---|---|
/// | 200 | 请求被理解并派发完成（`result` 或契约 `error` 包络） |
/// | 400 | 请求体不是合法 JSON / 缺 method |
/// | 401 | token 缺失或不正确 |
/// | 404 | 路径不对（只有 `POST /`） |
/// | 405 | 方法不对 |
pub const HTTP_PATHS: &str = "只有 POST /（JSON body）";

struct AppState {
    engine: Engine,
    token: String,
    shutdown: Arc<tokio::sync::Notify>,
}

/// 请求处理期间若被 drop（客户端断开），就取消在飞请求。
///
/// 外壳取消后 Rust 侧必须真的停下（`docs/01-ARCHITECTURE.md` §3 硬性设计 3）：
/// 靠 drop 触发比"轮询一个标志位"可靠——future 被丢掉的瞬间连接就关了。
struct CancelOnDrop(CancelToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// 绑定监听端口（`port = 0` 时由内核分配），返回 (listener, 实际端口)。
pub async fn bind(host: &str, port: u16) -> Result<(tokio::net::TcpListener, u16), String> {
    let listener = tokio::net::TcpListener::bind((host, port))
        .await
        .map_err(|e| format!("绑定 {host}:{port} 失败：{e}"))?;
    let actual = listener
        .local_addr()
        .map_err(|e| format!("读取本地端口失败：{e}"))?
        .port();
    Ok((listener, actual))
}

/// 起 HTTP 服务，直到 `shutdown` 被通知。
pub async fn serve_http(
    listener: tokio::net::TcpListener,
    engine: Engine,
    token: String,
    shutdown: Arc<tokio::sync::Notify>,
) -> Result<(), String> {
    let state = Arc::new(AppState {
        engine,
        token,
        shutdown: shutdown.clone(),
    });

    let app = Router::new()
        .route("/", post(handle))
        .fallback(fallback)
        .with_state(state);

    let waiter = {
        let shutdown = shutdown.clone();
        async move {
            shutdown.notified().await;
        }
    };
    axum::serve(listener, app)
        .with_graceful_shutdown(waiter)
        .await
        .map_err(|e| format!("HTTP 服务异常退出：{e}"))
}

async fn fallback() -> Response {
    (
        StatusCode::NOT_FOUND,
        axum::Json(serde_json::json!({
            "error": {
                "code": "invalid_request",
                "message": format!("未知路径；{HTTP_PATHS}"),
            }
        })),
    )
        .into_response()
}

async fn handle(State(state): State<Arc<AppState>>, headers: HeaderMap, body: String) -> Response {
    let cancel = CancelToken::new();
    let _guard = CancelOnDrop(cancel.clone());

    let req = match rpc::parse_request(&body) {
        Ok(req) => req,
        Err(envelope) => return (StatusCode::BAD_REQUEST, axum::Json(envelope)).into_response(),
    };

    let header_pairs: Vec<(String, String)> = headers
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_string(),
                String::from_utf8_lossy(v.as_bytes()).into_owned(),
            )
        })
        .collect();
    if let Err(envelope) = rpc::authorize(&req, &header_pairs, &state.token) {
        return (StatusCode::UNAUTHORIZED, axum::Json(envelope)).into_response();
    }

    let handled = rpc::dispatch(&state.engine, &req, &cancel).await;
    if handled.shutdown {
        tracing::info!("收到 system.shutdown，开始优雅退出");
        state.shutdown.notify_waiters();
    }
    (StatusCode::OK, axum::Json(handled.response)).into_response()
}

/// `--stdio`：每行一个 JSON 请求，每行一个 JSON 响应。
///
/// stdio 下不做 token 校验：管道本身就是父子进程之间的私有通道，
/// 再加一层 token 只会让调试变麻烦（HTTP 那边的 token 是因为**别的本机进程也能连**）。
///
/// EOF（父进程关掉管道）即退出——这正是"父进程死了子进程也要退"的天然实现。
pub async fn serve_stdio(engine: Engine, shutdown: Arc<tokio::sync::Notify>) -> Result<(), String> {
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();

    loop {
        let line = tokio::select! {
            _ = shutdown.notified() => break,
            line = lines.next_line() => match line {
                Ok(Some(line)) => line,
                Ok(None) => {
                    tracing::info!("stdin 已关闭，退出");
                    break;
                }
                Err(e) => {
                    tracing::error!("读取 stdin 失败：{e}");
                    break;
                }
            },
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let cancel = CancelToken::new();
        let _guard = CancelOnDrop(cancel.clone());
        let response = match rpc::parse_request(trimmed) {
            Ok(req) => {
                let handled = rpc::dispatch(&engine, &req, &cancel).await;
                if handled.shutdown {
                    tracing::info!("收到 system.shutdown，退出");
                    // 先把响应写出去再退
                    let mut rendered = serde_json::to_string(&handled.response)
                        .unwrap_or_else(|_| internal_error_json("响应无法序列化"));
                    rendered.push('\n');
                    let _ = stdout.write_all(rendered.as_bytes()).await;
                    let _ = stdout.flush().await;
                    break;
                }
                handled.response
            }
            Err(envelope) => envelope,
        };

        let mut rendered = match serde_json::to_string(&response) {
            Ok(s) => s,
            Err(_) => internal_error_json("响应无法序列化"),
        };
        rendered.push('\n');
        if let Err(e) = stdout.write_all(rendered.as_bytes()).await {
            tracing::error!("写 stdout 失败：{e}");
            break;
        }
        if let Err(e) = stdout.flush().await {
            tracing::error!("flush stdout 失败：{e}");
            break;
        }
    }
    Ok(())
}

fn internal_error_json(message: &str) -> String {
    serde_json::json!({ "error": XError::internal(message).to_object() }).to_string()
}

/// 把 ready 行写到 stdout 并 **flush**。
///
/// 必须 flush：stdout 重定向到管道时是块缓冲的，不 flush 外壳会一直等
/// （经典"握手挂住"）。
pub fn print_ready_line(port: u16, token: &str) -> Result<(), String> {
    use std::io::Write;
    let line = serde_json::json!({
        "port": port,
        "token": token,
        "version": xspider_core::CONTRACT_VERSION,
        "build": xspider_core::BUILD_VERSION,
    });
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "ready {line}").map_err(|e| format!("写 ready 行失败：{e}"))?;
    stdout
        .flush()
        .map_err(|e| format!("flush ready 行失败：{e}"))
}

/// 生成随机 token（32 字节 → base64url，无 padding）。
pub fn generate_token() -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    let bytes: [u8; 32] = rand::random();
    URL_SAFE_NO_PAD.encode(bytes)
}
