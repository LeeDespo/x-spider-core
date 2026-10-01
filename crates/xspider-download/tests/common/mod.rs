//! 本地 HTTP **fixture server**：`docs/04-TESTING-AND-FIXTURES.md` §5 要求的那个载体。
//!
//! 为什么必须自己写一个（而不是用现成的 mock 库）：
//! 需要精确控制**协议层**的行为，而 mock 库通常只给"返回一个响应"——
//! 这里要能造出：Range 分片、**发一半就断开**、声明大小与实际不符、
//! 忽略 Range、慢速流。这些正是下载代码最容易写错的地方。
//!
//! 用裸 TCP + 手写 HTTP/1.1：这样才能**随时把连接掐掉**（这是造"断流"的唯一办法）。
//! 同理，客户端的契约测试也用裸 TCP（`bins/xspiderd/tests/contract_dual.rs`）——
//! 两边都不引额外依赖，也就都能证明"我们说的是标准 HTTP"。

#![allow(dead_code)] // 不同的测试文件用到的场景不同

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// 服务端收到的请求（测试用来断言"真的带了 Range"）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub range: Option<String>,
}

#[derive(Debug, Default)]
struct State {
    requests: Mutex<Vec<RecordedRequest>>,
    /// `/flaky_once` 用：第一条连接是否已经被掐过
    flaky_fired: Mutex<bool>,
    /// 当前在飞的连接数，以及观察到的峰值。
    in_flight: AtomicUsize,
    peak_in_flight: AtomicUsize,
    completed_connections: AtomicUsize,
    /// **正在流式传输**的请求数（`/slow` 处理期间）。
    ///
    /// 为什么不用 socket 计数断言并发：TCP 层面还有"半关闭""连接池复用前的新连接"
    /// 这些噪音，它们会让 socket 计数虚高。要断言的是"同时在**下载**几个"。
    streaming: AtomicUsize,
    peak_streaming: AtomicUsize,
    /// 真正写出去的 body 字节数——用来断言 **HEAD / Range 探测没有下载文件**。
    bytes_sent: AtomicUsize,
}

impl State {
    fn enter(&self) {
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak_in_flight.fetch_max(now, Ordering::SeqCst);
    }
    fn leave(&self) {
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        self.completed_connections.fetch_add(1, Ordering::SeqCst);
    }
    fn stream_start(&self) {
        let now = self.streaming.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak_streaming.fetch_max(now, Ordering::SeqCst);
    }
    fn stream_end(&self) {
        self.streaming.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct FixtureServer {
    pub addr: SocketAddr,
    state: Arc<State>,
    handle: tokio::task::JoinHandle<()>,
}

impl FixtureServer {
    pub async fn start() -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("绑定随机端口失败");
        let addr = listener.local_addr().expect("读取端口失败");
        let state = Arc::new(State::default());
        let handle = {
            let state = state.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((socket, _)) = listener.accept().await else {
                        return;
                    };
                    let state = state.clone();
                    tokio::spawn(async move {
                        state.enter();
                        // 单个连接的失败不该影响其它连接
                        let _ = handle_connection(socket, state.clone()).await;
                        state.leave();
                    });
                }
            })
        };
        Self {
            addr,
            state,
            handle,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.state.requests.lock().unwrap().clone()
    }

    /// 观察到的**并发峰值**（当前在飞连接数的最大值）。并发上限测试用它。
    pub fn peak_in_flight(&self) -> usize {
        self.state.peak_in_flight.load(Ordering::SeqCst)
    }

    /// **同时下载中的请求数峰值**——并发上限测试用这个。
    pub fn peak_streaming(&self) -> usize {
        self.state.peak_streaming.load(Ordering::SeqCst)
    }

    /// 服务端写出去的 body 字节总数。
    ///
    /// 探测类断言靠它：HEAD 与 `bytes=0-0` 都**不该**把文件传下来。
    pub fn bytes_sent(&self) -> usize {
        self.state.bytes_sent.load(Ordering::SeqCst)
    }

    pub fn completed_connections(&self) -> usize {
        self.state.completed_connections.load(Ordering::SeqCst)
    }
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// 确定性的假字节：内容可预期，便于逐字节断言。
pub fn body_bytes(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

fn query_u64(path: &str, key: &str) -> Option<u64> {
    let query = path.split_once('?')?.1;
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=')?;
        if k == key {
            return v.parse().ok();
        }
    }
    None
}

fn path_only(path: &str) -> &str {
    path.split(['?', '#']).next().unwrap_or(path)
}

async fn handle_connection(mut socket: TcpStream, state: Arc<State>) -> std::io::Result<()> {
    // 读请求头（到空行为止）——测试里不需要 body
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let n = socket.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buffer.extend_from_slice(&chunk[..n]);
        if buffer.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buffer.len() > 16 * 1024 {
            return Ok(());
        }
    }
    let text = String::from_utf8_lossy(&buffer).into_owned();
    let mut lines = text.lines();
    let request_line = lines.next().unwrap_or_default().to_string();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_ascii_uppercase();
    let path = parts.next().unwrap_or("/").to_string();
    // HEAD 按协议不能回 body。**通用处理**（而不是在每个分支里各写一遍 if）——
    // `probe_size` 的第一条路径就是 HEAD，漏一个分支就会多传一整份文件。
    let is_head = method == "HEAD";
    let range = lines
        .find(|l| l.to_ascii_lowercase().starts_with("range:"))
        .and_then(|l| l.split_once(':').map(|(_, v)| v.trim().to_string()));

    state.requests.lock().unwrap().push(RecordedRequest {
        method: method.clone(),
        path: path.clone(),
        range: range.clone(),
    });

    let range_start = range
        .as_deref()
        .and_then(|r| r.strip_prefix("bytes="))
        .and_then(|r| r.split('-').next())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);

    match path_only(&path) {
        "/full" => {
            let size = query_u64(&path, "size").unwrap_or(1024) as usize;
            let body = body_bytes(size);
            if range_start > 0 && range_start < size as u64 {
                // 206：只发剩下的部分
                let slice = &body[range_start as usize..];
                write_response_for(
                    &mut socket,
                    is_head,
                    &state.bytes_sent,
                    206,
                    &[
                        ("Content-Length", slice.len().to_string()),
                        (
                            "Content-Range",
                            format!("bytes {}-{}/{}", range_start, size - 1, size),
                        ),
                    ],
                    slice,
                )
                .await
            } else {
                write_response_for(
                    &mut socket,
                    is_head,
                    &state.bytes_sent,
                    200,
                    &[("Content-Length", body.len().to_string())],
                    &body,
                )
                .await
            }
        }
        // 忽略 Range：永远返回 200 + 完整 body（客户端必须从头写，否则拼出错位文件）
        "/full_ignoring_range" => {
            let size = query_u64(&path, "size").unwrap_or(1024) as usize;
            let body = body_bytes(size);
            write_response_for(
                &mut socket,
                is_head,
                &state.bytes_sent,
                200,
                &[("Content-Length", body.len().to_string())],
                &body,
            )
            .await
        }
        // 每次都发一半就断开：**断流重试**用（最后一定成功不了，用来测重试上限与清理）
        "/always_cut" => {
            if is_head {
                let size = query_u64(&path, "size").unwrap_or(1024);
                return write_response_headers(
                    &mut socket,
                    200,
                    &[("Content-Length", size.to_string())],
                )
                .await;
            }
            let size = query_u64(&path, "size").unwrap_or(1024) as usize;
            let cut = query_u64(&path, "cut").unwrap_or(10) as usize;
            let body = body_bytes(size);
            write_response_headers(&mut socket, 200, &[("Content-Length", size.to_string())])
                .await?;
            // 只写一部分，然后**直接关连接** → 客户端看到 body 提前结束
            socket.write_all(&body[..cut.min(size)]).await?;
            socket.flush().await?;
            drop(socket);
            Ok(())
        }
        // 第一次截断、之后正常：**续传**用
        "/cut_once" => {
            if is_head {
                let size = query_u64(&path, "size").unwrap_or(1024);
                return write_response_headers(
                    &mut socket,
                    200,
                    &[("Content-Length", size.to_string())],
                )
                .await;
            }
            let already = *state.flaky_fired.lock().unwrap();
            let size = query_u64(&path, "size").unwrap_or(1024) as usize;
            let cut = query_u64(&path, "cut").unwrap_or(10) as usize;
            let body = body_bytes(size);
            if !already {
                *state.flaky_fired.lock().unwrap() = true;
                write_response_headers(&mut socket, 200, &[("Content-Length", size.to_string())])
                    .await?;
                socket.write_all(&body[..cut.min(size)]).await?;
                socket.flush().await?;
                drop(socket);
                return Ok(());
            }
            // 之后按正常实现（支持 Range）
            if range_start > 0 && range_start < size as u64 {
                let slice = &body[range_start as usize..];
                write_response_for(
                    &mut socket,
                    is_head,
                    &state.bytes_sent,
                    206,
                    &[
                        ("Content-Length", slice.len().to_string()),
                        (
                            "Content-Range",
                            format!("bytes {}-{}/{}", range_start, size - 1, size),
                        ),
                    ],
                    slice,
                )
                .await
            } else {
                write_response_for(
                    &mut socket,
                    is_head,
                    &state.bytes_sent,
                    200,
                    &[("Content-Length", body.len().to_string())],
                    &body,
                )
                .await
            }
        }
        // 声明大小与实际不符：Content-Length 写着 declared，实际只发 actual
        "/liar" => {
            if is_head {
                let declared = query_u64(&path, "declared").unwrap_or(100);
                return write_response_headers(
                    &mut socket,
                    200,
                    &[("Content-Length", declared.to_string())],
                )
                .await;
            }
            let declared = query_u64(&path, "declared").unwrap_or(100) as usize;
            let actual = query_u64(&path, "actual").unwrap_or(40) as usize;
            let body = body_bytes(actual);
            write_response_headers(
                &mut socket,
                200,
                &[("Content-Length", declared.to_string())],
            )
            .await?;
            socket.write_all(&body).await?;
            socket.flush().await?;
            drop(socket);
            Ok(())
        }
        // 只支持 HEAD 的场景：用来验证第一条路径能拿到 Content-Length
        "/head_only" => {
            let size = query_u64(&path, "size").unwrap_or(1024) as usize;
            write_response_headers(&mut socket, 200, &[("Content-Length", size.to_string())]).await
        }
        // HEAD 明确不支持（405）+ Range 支持：验证"退回 1 字节 Range"这条路
        "/no_head" => {
            let size = query_u64(&path, "size").unwrap_or(1024) as usize;
            if method == "HEAD" {
                return write_response_for(
                    &mut socket,
                    is_head,
                    &state.bytes_sent,
                    405,
                    &[("Content-Length", "0".to_string())],
                    &[],
                )
                .await;
            }
            // 无条件的 206：真实 CDN 上 Range 总是被支持，这里只保证"能问出总长"
            write_response_for(
                &mut socket,
                is_head,
                &state.bytes_sent,
                206,
                &[
                    ("Content-Length", "1".to_string()),
                    ("Content-Range", format!("bytes 0-0/{size}")),
                ],
                &body_bytes(1),
            )
            .await
        }
        "/missing" => {
            write_response_for(
                &mut socket,
                is_head,
                &state.bytes_sent,
                404,
                &[("Content-Length", "0".to_string())],
                &[],
            )
            .await
        }
        "/forbidden" => {
            write_response_for(
                &mut socket,
                is_head,
                &state.bytes_sent,
                403,
                &[("Content-Length", "0".to_string())],
                &[],
            )
            .await
        }
        "/boom" => {
            write_response_for(
                &mut socket,
                is_head,
                &state.bytes_sent,
                500,
                &[("Content-Length", "0".to_string())],
                &[],
            )
            .await
        }
        // 慢速：每 50ms 发 1KB。既用来测取消，也用来测"暂停后从断点续传"
        "/slow" => {
            if is_head {
                let size = query_u64(&path, "size").unwrap_or(64 * 1024);
                return write_response_headers(
                    &mut socket,
                    200,
                    &[("Content-Length", size.to_string())],
                )
                .await;
            }
            // 计数只覆盖"真的在写字节"的区间——**不要在最后一次写完后再 sleep**，
            // 否则服务端的计数会比客户端的"下载结束"晚 50ms，测出来的并发虚高。
            return slow_response(&mut socket, &path, range_start, &state).await;
        }
        _ => {
            write_response_for(
                &mut socket,
                is_head,
                &state.bytes_sent,
                404,
                &[("Content-Length", "0".to_string())],
                &[],
            )
            .await
        }
    }
}

/// `/slow`：每 50ms 发 1KB，支持 Range（这样"暂停后恢复"也能用它测）。
async fn slow_response(
    socket: &mut TcpStream,
    path: &str,
    range_start: u64,
    state: &State,
) -> std::io::Result<()> {
    let size = query_u64(path, "size").unwrap_or(64 * 1024) as usize;
    let body = body_bytes(size);
    let start = (range_start as usize).min(size);

    if start > 0 {
        // 206：只发剩下的部分（"暂停后恢复"要靠它）
        let slice = &body[start..];
        write_response_headers(
            socket,
            206,
            &[
                ("Content-Length", slice.len().to_string()),
                (
                    "Content-Range",
                    format!("bytes {}-{}/{}", start, size - 1, size),
                ),
            ],
        )
        .await?;
        state.stream_start();
        let result = stream_pieces(socket, slice).await;
        state.stream_end();
        return result;
    }

    write_response_headers(socket, 200, &[("Content-Length", size.to_string())]).await?;
    state.stream_start();
    let result = stream_pieces(socket, &body).await;
    state.stream_end();
    result
}

/// 每 50ms 发 1KB——慢到足以观察并发与暂停。
///
/// 节流放在**每次写之前**（第一片除外）：这样最后一片写完就返回，
/// "服务端还在写"与"客户端已读完"之间不会凭空多出一个 50ms 的重叠窗口。
async fn stream_pieces(socket: &mut TcpStream, body: &[u8]) -> std::io::Result<()> {
    for (index, piece) in body.chunks(1024).enumerate() {
        if index > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        if socket.write_all(piece).await.is_err() {
            return Ok(());
        }
        if socket.flush().await.is_err() {
            return Ok(());
        }
    }
    Ok(())
}

/// HEAD 只回头；其余方法照常写体。
async fn write_response_for(
    socket: &mut TcpStream,
    is_head: bool,
    bytes_written: &AtomicUsize,
    status: u16,
    headers: &[(&str, String)],
    body: &[u8],
) -> std::io::Result<()> {
    write_response_headers(socket, status, headers).await?;
    if is_head {
        return Ok(());
    }
    bytes_written.fetch_add(body.len(), Ordering::SeqCst);
    socket.write_all(body).await?;
    socket.flush().await
}

async fn write_response_headers(
    socket: &mut TcpStream,
    status: u16,
    headers: &[(&str, String)],
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        206 => "Partial Content",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Status",
    };
    let mut response = format!("HTTP/1.1 {status} {reason}\r\n");
    for (name, value) in headers {
        response.push_str(&format!("{name}: {value}\r\n"));
    }
    // 一律关连接：这样"服务端发一半就断开"和"正常结束"在客户端看起来只是 body 长度不同
    response.push_str("Connection: close\r\n\r\n");
    socket.write_all(response.as_bytes()).await?;
    socket.flush().await
}
