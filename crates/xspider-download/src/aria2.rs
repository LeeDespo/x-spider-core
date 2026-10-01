//! **Aria2Next 外派后端**：用子进程 + JSON-RPC 搬字节（`docs/01-ARCHITECTURE.md` §5.2）。
//!
//! # 为什么是 **Aria2Next**，不是上游 aria2
//!
//! 本仓库只支持 [`AnInsomniacy/aria2-next`](https://github.com/AnInsomniacy/aria2-next)
//! （`docs/03-FFI-SIGNING-PACKAGING.md` §4 指定的那个 fork）。这不是洁癖，是**实测差异**：
//!
//! | 事实 | 实测（Aria2Next 2.7.5，2026-10-01） |
//! |---|---|
//! | 产品标识 | `aria2.getVersion` 返回 `product: "aria2-next"` |
//! | 选项集与上游不同 | `--split` / `--max-connection-per-server` **不在 `--help=#all` 里**，但通过 RPC 读得到（值 6）→ **只能实测，不能看帮助** |
//! | fork 专有项 | `--stream-max-connections`（"per-file HTTP connection ceiling"） |
//! | 许可 | GPL-2.0（v2 or later） |
//!
//! 所以 [`Aria2Next::spawn`] 会**校验 `product` 必须是 `aria2-next`**：指向上游 aria2
//! 会被明确拒绝，而不是带着一套不同的行为静默跑起来。
//!
//! # 实测踩到的两个坑（实现就是围绕它们写的）
//!
//! 1. **404 会被报成"成功"**。实测：请求一个不存在的 URL，`tellStatus` 返回
//!    `status: "complete", errorCode: 0, completedLength: 0, totalLength: 0`，
//!    并且在磁盘上留下一个 **0 字节文件**。这正是 `docs/02` §E2 说的那件事——
//!    **完成判据必须是"落盘字节数"，不能是"引擎说成功了"**。
//! 2. **RPC 层的错误全是 `code: 1`**（`Unknown option` / `GID ... is not found` /
//!    `Unauthorized` 都一样），所以 RPC 错误**没法按码分类**。对策：
//!    自己把选项名钉死（不依赖服务端拒绝），并把手上的 GID 记账在本地——
//!    对 RPC 错误一律当"后端异常"上报，`message` 只作诊断，**不参与任何判断**。
//!
//! # 归属纪律
//!
//! 目录与文件名**由外壳算好再传**（`docs/01` §4）；引擎专有调优（`stream-max-connections`
//! 之类）只在这里出现，**绝不进契约**（`docs/01` §5.2 纪律 2）。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::process::{Child, Command};

use xspider_core::cancel::CancelToken;
use xspider_core::ratelimit::Limits;

use crate::http_backend::{
    DownloadError, DownloadOutcome, DownloadRequest, DownloadResult, ProgressFn,
};
use crate::http_backend::{Integrity, ATTRIBUTION_ARIA2, DOWNLOAD_REFERER, DOWNLOAD_USER_AGENT};

/// RPC 就绪等待上限。冷启动实测在 1 秒内，给足余量。
const READY_TIMEOUT: Duration = Duration::from_secs(15);
/// 轮询 `tellStatus` 的间隔。aria2 自己是异步的，这里只需要"够快能看到进度"。
const POLL_INTERVAL: Duration = Duration::from_millis(200);
/// 优雅关停的等待上限，超时就强杀（不能留僵尸进程，`docs/05` §6）。
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
/// 单次 RPC 的 HTTP 超时。
const RPC_TIMEOUT: Duration = Duration::from_secs(10);
/// 启动重试次数（端口争用是"先探测再绑定"的固有窗口，见 `free_port`）。
const SPAWN_ATTEMPTS: u32 = 3;

/// 启动参数。
#[derive(Debug, Clone)]
pub struct Aria2NextConfig {
    /// 二进制路径。用 [`Self::locate_binary`] 找一个。
    pub binary: PathBuf,
    /// 默认下载目录（每个请求还会用 `dir` 覆盖成目标文件所在目录）。
    pub dir: PathBuf,
    /// RPC 端口。`0` = 让系统分配。
    pub port: u16,
    /// RPC 密钥。空 = 自动生成随机值。
    pub secret: String,
    /// 引擎侧并发上限。默认取 [`Limits::cdn_concurrency`]。
    pub max_concurrent: u32,
    /// **逃生舱**：引擎专有调优参数（例如 `--stream-max-connections=8`）。
    ///
    /// 它存在的意义是"不改代码也能调引擎"，而它**永远不进契约**——
    /// 一旦进了，契约就被焊死在 aria2-next 的选项集上（`docs/01` §5.2 纪律 2）。
    pub extra_args: Vec<String>,
}

impl Aria2NextConfig {
    pub fn new(binary: impl Into<PathBuf>, dir: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            dir: dir.into(),
            port: 0,
            secret: String::new(),
            max_concurrent: Limits::default().cdn_concurrency,
            extra_args: Vec::new(),
        }
    }

    /// 找一个可用的 Aria2Next 二进制。
    ///
    /// 顺序：`XSPIDER_ARIA2_PATH` → 本机 `x-spider-mac` 里随包的那个 →
    /// `PATH` 里的 `aria2next` / `aria2-next`。
    ///
    /// **刻意不找 `aria2c`**：上游 aria2 不在支持范围内（见模块头注释）。
    pub fn locate_binary() -> Option<PathBuf> {
        if let Ok(path) = std::env::var("XSPIDER_ARIA2_PATH") {
            let path = PathBuf::from(path.trim());
            if path.is_file() {
                return Some(path);
            }
        }
        for candidate in bundled_candidates() {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        let path_var = std::env::var_os("PATH")?;
        for dir in std::env::split_paths(&path_var) {
            for name in ["aria2next", "aria2-next"] {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        None
    }

    /// 拼出命令行参数。**抽成独立函数是为了能离线测**（真实启动依赖二进制）。
    ///
    /// 选项集合与参考实现（`x-spider-mac/.../Aria2RPCClient.swift` 的启动参数）对齐，
    /// 逐条都有理由：
    /// - `--conf-path=/dev/null` + `--no-conf`：**清空**用户机器上的 aria2 配置。
    ///   组件的行为必须只由这里决定——否则"我这台机器上能下、那台不行"会变成玄学；
    /// - `--enable-dht*` / `--bt-*` / `--enable-peer-exchange=false`：这是 HTTP 下载器，
    ///   不开 BT 的 DHT/LPD/PEX。除了省资源，还避免**在用户机器上监听 UDP 端口**
    ///   （那会触发防火墙弹窗，见 `docs/03` §6 的 Windows 特有事项）；
    /// - `--continue` / `--auto-file-renaming=false` / `--allow-overwrite=true`：
    ///   断点续传 + 精确文件名（addUri 里还会再给一遍，这里是兜底，
    ///   避免"某次 RPC 选项没生效"就静默改成了别的文件名）。
    pub fn to_args(&self, port: u16, secret: &str) -> Vec<String> {
        let mut args = vec![
            // 不读用户/系统的 aria2 配置：组件的行为必须只由这里决定
            "--no-conf=true".to_string(),
            "--conf-path=/dev/null".to_string(),
            "--enable-rpc=true".to_string(),
            // 只监听回环：本机其它进程都不该直接连它，更别说局域网
            "--rpc-listen-all=false".to_string(),
            format!("--rpc-listen-port={port}"),
            format!("--rpc-secret={secret}"),
            format!("--dir={}", self.dir.display()),
            format!("--max-concurrent-downloads={}", self.max_concurrent.max(1)),
            // 只要 HTTP：把 BT 相关的发现机制全关掉
            "--enable-dht=false".to_string(),
            "--enable-dht6=false".to_string(),
            "--bt-enable-lpd=false".to_string(),
            "--enable-peer-exchange=false".to_string(),
            // 断点与文件名的默认语义（addUri 里逐任务再覆盖）
            "--continue=true".to_string(),
            "--auto-file-renaming=false".to_string(),
            "--allow-overwrite=true".to_string(),
            // stdout/stderr 的噪音我们自己控：日志级别压到 warn，进度汇总关掉
            "--quiet=true".to_string(),
            "--console-log-level=warn".to_string(),
            "--summary-interval=0".to_string(),
            // 父进程没了就自己退（第二道保险，第一道是 kill_on_drop）
            format!("--stop-with-process={}", std::process::id()),
        ];
        args.extend(self.extra_args.iter().cloned());
        args
    }
}

/// 一个活着的 Aria2Next 进程 + 它的 RPC 客户端。
pub struct Aria2Next {
    child: Child,
    port: u16,
    secret: String,
    version: String,
    client: reqwest::Client,
}

impl std::fmt::Debug for Aria2Next {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Aria2Next")
            .field("version", &self.version)
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

impl Aria2Next {
    /// 启动并完成握手：`--version` 预检 → 起进程 → 等 RPC 就绪 → **校验产品标识**。
    pub async fn spawn(mut config: Aria2NextConfig) -> DownloadResult<Self> {
        // 预检：二进制存在吗？是不是 Aria2Next？（预检失败时不需要起进程，报错也更快）
        preflight(&config.binary).await?;

        if config.secret.trim().is_empty() {
            config.secret = random_secret();
        }

        let client = reqwest::Client::builder()
            .timeout(RPC_TIMEOUT)
            // RPC 走回环，绝不能因此去连代理
            .no_proxy()
            .build()
            .map_err(|e| DownloadError::Io {
                kind: "client",
                message: format!("构造 RPC 客户端失败：{e}"),
            })?;

        // 端口是"先探测再绑定"（见 free_port 的说明），存在极小的争用窗口。
        // 所以**换端口重试**而不是一次失败就放弃——这个窗口在真实使用里也会遇到
        // （别的进程刚好抢到），而在测试里并发启动多个引擎时更容易撞上。
        let mut last_error: Option<DownloadError> = None;
        for attempt in 1..=SPAWN_ATTEMPTS {
            let port = if config.port == 0 {
                free_port()?
            } else {
                config.port
            };
            let args = config.to_args(port, &config.secret);
            tracing::debug!(binary = %config.binary.display(), port, attempt, "启动 Aria2Next");

            let child = Command::new(&config.binary)
                .args(&args)
                .stdin(Stdio::null())
                // 引擎的输出不混进我们的 stdout（sidecar 的 stdout 是握手通道）
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                // 第一道保险：本进程被 drop/panic 时子进程一起走
                .kill_on_drop(true)
                .spawn()
                .map_err(|e| DownloadError::Io {
                    kind: "spawn_failed",
                    message: format!("启动 {} 失败：{e}", config.binary.display()),
                })?;

            let mut engine = Self {
                child,
                port,
                secret: config.secret.clone(),
                version: String::new(),
                client: client.clone(),
            };

            match engine.wait_ready().await {
                Ok(version) => {
                    engine.version = version;
                    return Ok(engine);
                }
                Err(e) => {
                    // 起不来的话把 stderr 捞出来当诊断（只作诊断，不作判断）
                    let detail = engine.drain_stderr().await;
                    let error = match e {
                        DownloadError::Transport { kind, message } => DownloadError::Transport {
                            kind,
                            message: format!("{message}；引擎输出：{detail}"),
                        },
                        other => other,
                    };
                    // 参数/产品不对这类问题换端口也没用，直接返回
                    if matches!(error, DownloadError::Invalid { .. }) {
                        return Err(error);
                    }
                    tracing::debug!(attempt, error = %error, "引擎没起来，换端口重试");
                    last_error = Some(error);
                }
            }
        }
        Err(last_error.unwrap_or(DownloadError::Transport {
            kind: "engine_start",
            message: "Aria2Next 启动失败".to_string(),
        }))
    }

    /// 实测出来的版本（来自 `aria2.getVersion`，不是猜的）。
    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// 轮询 `aria2.getVersion` 直到就绪。
    async fn wait_ready(&mut self) -> DownloadResult<String> {
        let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
        loop {
            // 进程已经死了就没必要再等
            if let Ok(Some(status)) = self.child.try_wait() {
                return Err(DownloadError::Transport {
                    kind: "engine_exited",
                    message: format!("Aria2Next 启动后立即退出（{status}）"),
                });
            }
            if let Ok(value) = self.call("aria2.getVersion", json!([])).await {
                let product = value
                    .get("product")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !product.contains("aria2-next") {
                    return Err(DownloadError::Invalid {
                        message: format!(
                            "只支持 Aria2Next（应报告 product=aria2-next），实际 product={product:?}。\
                             上游 aria2 的选项集与行为都不同，不在支持范围内。"
                        ),
                    });
                }
                return Ok(value
                    .get("version")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(DownloadError::Transport {
                    kind: "rpc_timeout",
                    message: format!("{READY_TIMEOUT:?} 内没有等到 RPC 就绪（端口 {})", self.port),
                });
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// 优雅关停：先 `aria2.shutdown`，超时再强杀。
    pub async fn shutdown(&mut self) -> DownloadResult<()> {
        // 引擎可能已经自己退了
        if let Ok(Some(_)) = self.child.try_wait() {
            return Ok(());
        }
        let _ = self.call("aria2.shutdown", json!([])).await;
        match tokio::time::timeout(SHUTDOWN_TIMEOUT, self.child.wait()).await {
            Ok(Ok(_)) => Ok(()),
            _ => {
                // 超时或出错 → 强杀，绝不留僵尸（docs/05 §6）
                let _ = self.child.start_kill();
                let _ = self.child.wait().await;
                Ok(())
            }
        }
    }

    async fn drain_stderr(&mut self) -> String {
        use tokio::io::AsyncReadExt;
        let Some(mut stderr) = self.child.stderr.take() else {
            return "<无输出>".to_string();
        };
        let mut buffer = Vec::new();
        let _ =
            tokio::time::timeout(Duration::from_millis(300), stderr.read_to_end(&mut buffer)).await;
        let text = String::from_utf8_lossy(&buffer);
        text.chars().take(400).collect()
    }

    /// 发一次 JSON-RPC。`method` 与 `params` 都是 aria2 的协议面（`docs/03` §4：协议才是跨端统一的那部分）。
    async fn call(&self, method: &str, params: Value) -> DownloadResult<Value> {
        let mut full = vec![json!(format!("token:{}", self.secret))];
        if let Some(array) = params.as_array() {
            full.extend(array.iter().cloned());
        }
        let body = json!({ "jsonrpc": "2.0", "id": "xspider", "method": method, "params": full });
        let response = self
            .client
            .post(format!("http://127.0.0.1:{}/jsonrpc", self.port))
            .json(&body)
            .send()
            .await
            .map_err(|e| DownloadError::Transport {
                kind: if e.is_connect() { "connect" } else { "rpc" },
                message: format!("RPC {method} 失败：{e}"),
            })?;
        let value: Value = response
            .json()
            .await
            .map_err(|e| DownloadError::Transport {
                kind: "rpc_body",
                message: format!("RPC {method} 响应不是 JSON：{e}"),
            })?;
        if let Some(error) = value.get("error") {
            // **不按码分类**：实测 aria2-next 的 RPC 错误全是 code:1。
            // 所以这里一律当"后端异常"，`message` 只作诊断（铁律：不用文案做判断）。
            return Err(DownloadError::Transport {
                kind: "rpc_error",
                message: format!(
                    "RPC {method} 返回错误（code={}）：{}",
                    error.get("code").and_then(Value::as_i64).unwrap_or(-1),
                    error.get("message").and_then(Value::as_str).unwrap_or("?")
                ),
            });
        }
        Ok(value.get("result").cloned().unwrap_or(Value::Null))
    }

    /// 下载一个 URL 到一个文件。语义与 [`crate::http_backend::HttpDownloader::download`] 对齐。
    ///
    /// 三条与引擎无关的纪律照旧由**我们**负责（`docs/01` §5.2 纪律 3）：
    /// 目录/文件名由调用方给定、完整性由我们校验、临时文件由我们清理与改名。
    pub async fn download(
        &self,
        request: &DownloadRequest,
        cancel: &CancelToken,
    ) -> DownloadResult<DownloadOutcome> {
        self.download_with_progress(request, cancel, &mut |_, _| {})
            .await
    }

    /// 与 [`Self::download`] 相同，但会报告进度（来自 `tellStatus` 的轮询）。
    pub async fn download_with_progress(
        &self,
        request: &DownloadRequest,
        cancel: &CancelToken,
        progress: ProgressFn<'_>,
    ) -> DownloadResult<DownloadOutcome> {
        if request.url.trim().is_empty() {
            return Err(DownloadError::Invalid {
                message: "url 不能为空".to_string(),
            });
        }
        let dir = request
            .dest_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        std::fs::create_dir_all(&dir).map_err(|e| DownloadError::Io {
            kind: "create_dir",
            message: format!("创建目录 {} 失败：{e}", dir.display()),
        })?;

        // 引擎写的临时名是**引擎专有**的：与内置后端的断点互不通用，
        // 所以名字里带上引擎标识，物理上杜绝"换个引擎接着用半成品"
        // （docs/02 §E3：引擎切换必须丢弃断点，拼在一起会产出损坏文件）。
        let part_name = part_file_name(&request.dest_path, ATTRIBUTION_ARIA2);
        let part_path = dir.join(&part_name);

        // 每个下载自己的选项。**这里出现的选项名是与 Aria2Next 的私有约定，
        // 绝不进契约**（docs/01 §5.2 纪律 2）。
        let options = per_request_options(request, &dir, &part_name);

        let gid = self
            .call("aria2.addUri", json!([[request.url], options]))
            .await?
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| DownloadError::Transport {
                kind: "rpc_shape",
                message: "aria2.addUri 没有返回 GID".to_string(),
            })?;

        let outcome = self
            .poll_until_done(&gid, &part_path, request, cancel, progress)
            .await;

        // 无论成败都把任务从引擎列表里摘掉（否则长跑会越积越多）
        let _ = self.call("aria2.removeDownloadResult", json!([gid])).await;

        match outcome {
            Ok(outcome) => Ok(outcome),
            Err(e) => {
                // 取消时的处理与"暂停"不同：暂停要留着断点给下次续传
                let keep = matches!(e, DownloadError::Cancelled) && request.keep_partial_on_cancel;
                if keep {
                    tracing::debug!(path = %part_path.display(), "取消但保留断点（暂停语义）");
                } else {
                    // 失败/放弃都要清理：引擎留下的可能是 0 字节文件或半成品（docs/02 §E4）
                    cleanup_engine_files(&part_path);
                }
                Err(e)
            }
        }
    }

    async fn poll_until_done(
        &self,
        gid: &str,
        part_path: &Path,
        request: &DownloadRequest,
        cancel: &CancelToken,
        progress: ProgressFn<'_>,
    ) -> DownloadResult<DownloadOutcome> {
        let keys = [
            "gid",
            "status",
            "totalLength",
            "completedLength",
            "errorCode",
            "errorMessage",
            "files",
        ];
        loop {
            if cancel.is_cancelled() {
                // 让引擎也停：remove 是"软"的，forceRemove 更可靠
                let _ = self.call("aria2.forceRemove", json!([gid])).await;
                return Err(DownloadError::Cancelled);
            }
            let status = self.call("aria2.tellStatus", json!([gid, keys])).await?;
            let state = status
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown");

            // 进度来自引擎的 completedLength/totalLength（都是字符串形式的数字）
            let done = status
                .get("completedLength")
                .and_then(Value::as_str)
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);
            let total = status
                .get("totalLength")
                .and_then(Value::as_str)
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);
            progress(done, total);

            match state {
                "complete" => return self.finish(request, part_path, &status),
                "error" => {
                    let code = status
                        .get("errorCode")
                        .and_then(Value::as_str)
                        .and_then(|c| c.parse::<i64>().ok())
                        .unwrap_or(0);
                    let message = status
                        .get("errorMessage")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    return Err(map_aria2_error_code(code, &message));
                }
                "removed" => return Err(DownloadError::Cancelled),
                // active / waiting / paused：继续等
                _ => {}
            }
            match cancel.race(tokio::time::sleep(POLL_INTERVAL)).await {
                Ok(()) => {}
                Err(_) => {
                    let _ = self.call("aria2.forceRemove", json!([gid])).await;
                    return Err(DownloadError::Cancelled);
                }
            }
        }
    }

    /// 校验 + 改名。**这一层与引擎无关**，所以内置后端与 aria2 后端走同一套判断。
    fn finish(
        &self,
        request: &DownloadRequest,
        part_path: &Path,
        status: &Value,
    ) -> DownloadResult<DownloadOutcome> {
        let completed = status
            .get("completedLength")
            .and_then(Value::as_str)
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        let total = status
            .get("totalLength")
            .and_then(Value::as_str)
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);

        let actual = std::fs::metadata(part_path).map(|m| m.len()).unwrap_or(0);

        // **实测坑**：aria2-next 对 404 会报 status=complete + errorCode=0，
        // 并在磁盘上留下 **0 字节文件**。所以"引擎说完成"完全不能当完成。
        if actual == 0 && request.expect_size != Some(0) {
            return Err(DownloadError::IntegrityFailed {
                expected: request.expect_size.unwrap_or(total),
                actual,
            });
        }

        let integrity = match request.expect_size {
            Some(expected) if actual != expected => {
                return Err(DownloadError::IntegrityFailed { expected, actual })
            }
            Some(expected) => Integrity::Verified { expected, actual },
            None => Integrity::Unverified { actual },
        };

        if actual != completed && completed != 0 {
            tracing::debug!(actual, completed, "引擎报告的字节数与落盘不一致");
        }

        std::fs::rename(part_path, &request.dest_path).map_err(|e| DownloadError::Io {
            kind: "rename",
            message: format!(
                "落盘 {} → {} 失败：{e}",
                part_path.display(),
                request.dest_path.display()
            ),
        })?;
        cleanup_engine_files(part_path);

        Ok(DownloadOutcome {
            bytes: actual,
            resumed_from: 0,
            attempts: 1,
            integrity,
        })
    }
}

impl Drop for Aria2Next {
    fn drop(&mut self) {
        // kill_on_drop 已经覆盖了，这里只是再明确一次意图：
        // **绝不留残留子进程**（docs/05 §6：冒烟脚本要断言 pgrep 为空）
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.start_kill();
        }
    }
}

/// 预检：文件在不在、是不是 Aria2Next（用 `--version`，不必起进程）。
async fn preflight(binary: &Path) -> DownloadResult<()> {
    if !binary.is_file() {
        return Err(DownloadError::Invalid {
            message: format!(
                "找不到 Aria2Next 二进制：{}（可用 XSPIDER_ARIA2_PATH 指定）",
                binary.display()
            ),
        });
    }
    let output = Command::new(binary)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| DownloadError::Io {
            kind: "spawn_failed",
            message: format!("执行 {} --version 失败：{e}", binary.display()),
        })?;
    let text = String::from_utf8_lossy(&output.stdout);
    let looks_like_aria2next = text.to_ascii_lowercase().contains("aria2 next")
        || text.to_ascii_lowercase().contains("aria2-next");
    if !looks_like_aria2next {
        return Err(DownloadError::Invalid {
            message: format!(
                "{} 看起来不是 Aria2Next（`--version` 第一行：{}）。\
                 本组件只支持 Aria2Next，不支持上游 aria2。",
                binary.display(),
                text.lines().next().unwrap_or("<空>")
            ),
        });
    }
    Ok(())
}

/// aria2 的数字错误码 → 我们的结构化原因。
///
/// 用的是 aria2 官方文档里的**数字枚举**（不是文案）。映射不到的码归到
/// `Upstream { status: 0 }` 并把码带在消息里作诊断——**不做文案匹配**。
fn map_aria2_error_code(code: i64, message: &str) -> DownloadError {
    match code {
        1 => DownloadError::Transport {
            kind: "aria2_unknown",
            message: format!("引擎报未知错误：{message}"),
        },
        // 3 = resource not found
        3 => DownloadError::NotFound,
        // 8 = remote server did not support resume（断点不通用）——当作传输问题可重试
        8 => DownloadError::Transport {
            kind: "range_unsupported",
            message: format!("服务端不支持断点续传：{message}"),
        },
        // 16 = file already exists
        16 => DownloadError::Upstream { status: 0 },
        // 24 = HTTP authorization failed
        24 => DownloadError::AuthRequired { status: 403 },
        // 其余码不猜语义：归为传输类失败，并把**数字码**带在消息里作诊断。
        // （不按文案判断——码是结构化信息，文案不是。）
        other => DownloadError::Transport {
            kind: "aria2_error",
            message: format!("引擎报错 errorCode={other}：{message}"),
        },
    }
}

/// "随包带"的几个候选位置。**顺序即优先级**：
///
/// 1. **`xspiderd` 自己旁边**——这是主推的部署方式：组件目录里放
///    `xspiderd` + `aria2next` 两个文件，换组件就只是换这两个文件
///    （`docs/07-API-REFERENCE.md` §2）；
/// 2. 参考实现的 App bundle 里那份（开发机上顺手能用上）；
/// 3. 当前工作目录下 `x-spider-mac/...`（在仓库根目录跑测试时的常见位置）。
///
/// 都用**相对位置**拼，不写死某个人的家目录。
fn bundled_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("aria2next"));
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(
            PathBuf::from(home)
                .join("Documents/x-spider-mac/XSpiderMac/Resources/Binaries/aria2next"),
        );
    }
    candidates.push(PathBuf::from(
        "x-spider-mac/XSpiderMac/Resources/Binaries/aria2next",
    ));
    candidates
}

/// 一次 `aria2.addUri` 的逐任务选项。
///
/// 抽成独立函数是为了能**离线测**（真实调用要起进程），也为了让"我们到底给引擎
/// 传了什么"只有一处可读。
///
/// `user-agent` / `referer` 与参考实现（`x-spider-mac/.../Aria2Engine.swift`）
/// 逐字一致：媒体 CDN 对着默认 UA 的态度与浏览器不同，而 aria2 自己只带默认 UA。
/// `segments > 1` → 单文件多连接，选项名是 **`stream-max-connections`**：
/// 上游 aria2 的 `--split` / `--max-connection-per-server` 在 Aria2Next 里已退役
/// （实测 `--help` 里根本没有，见模块头注释），照抄旧名字等于没设。
fn per_request_options(
    request: &DownloadRequest,
    dir: &Path,
    part_name: &str,
) -> serde_json::Map<String, Value> {
    let mut options = serde_json::Map::new();
    options.insert("dir".into(), json!(dir.display().to_string()));
    options.insert("out".into(), json!(part_name));
    options.insert("continue".into(), json!("true"));
    options.insert("auto-file-renaming".into(), json!("false"));
    options.insert("allow-overwrite".into(), json!("true"));
    options.insert("user-agent".into(), json!(DOWNLOAD_USER_AGENT));
    options.insert("referer".into(), json!(DOWNLOAD_REFERER));
    // 子进程不认识"跟随环境变量"，代理必须是具体 URL（`ProxyConfig::resolve_url`）。
    // 与参考实现一致：它也把当前代理逐任务传给 aria2（`options["all-proxy"]`）。
    if let Some(proxy) = &request.proxy_url {
        options.insert("all-proxy".into(), json!(proxy));
    }
    if request.segments > 1 {
        options.insert(
            "stream-max-connections".into(),
            json!(request.segments.to_string()),
        );
    }
    options
}

/// 引擎写的临时文件名（带引擎标识，见 `download` 里的说明）。
fn part_file_name(dest: &Path, attribution: &str) -> String {
    format!(
        "{}.part.{attribution}",
        dest.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "download".to_string())
    )
}

/// 清掉引擎留下的东西：半成品本体 + aria2 的 `.aria2` 控制文件（`docs/02` §E4）。
fn cleanup_engine_files(part_path: &Path) {
    for path in [part_path.to_path_buf(), with_aria2_suffix(part_path)] {
        if let Err(e) = std::fs::remove_file(&path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(path = %path.display(), error = %e, "清理引擎文件失败");
            }
        }
    }
}

fn with_aria2_suffix(part_path: &Path) -> PathBuf {
    let mut name = part_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.push_str(".aria2");
    part_path.with_file_name(name)
}

/// 随机 RPC 密钥。**只在本机回环用**，但仍然是随机值：
/// 本机其它进程（包括别的用户会话）不该能随便指挥我们的引擎。
fn random_secret() -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    let bytes: [u8; 24] = rand::random();
    URL_SAFE_NO_PAD.encode(bytes)
}

/// 让内核分配一个空闲端口，然后立刻释放——用它给 aria2 的 RPC 用。
///
/// 这是"先探测再绑定"的常见做法，存在极小的争用窗口；但 READY_TIMEOUT 里
/// 会检查进程是否还活着，所以争用会以"启动失败"的形式暴露，而不是静默跑错端口。
fn free_port() -> DownloadResult<u16> {
    let listener =
        std::net::TcpListener::bind(("127.0.0.1", 0)).map_err(|e| DownloadError::Io {
            kind: "bind",
            message: format!("分配 RPC 端口失败：{e}"),
        })?;
    let port = listener.local_addr().map_err(|e| DownloadError::Io {
        kind: "bind",
        message: format!("读取端口失败：{e}"),
    })?;
    Ok(port.port())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Aria2NextConfig {
        Aria2NextConfig::new("/tmp/aria2next", "/tmp/dl")
    }

    #[test]
    fn args_contain_only_verified_options() {
        let args = config().to_args(6800, "s3cret");
        // 这几个都是实测能从 getGlobalOption 读到的选项（不是看帮助猜的）
        for expected in [
            "--no-conf=true",
            "--enable-rpc=true",
            "--rpc-listen-all=false",
            "--rpc-listen-port=6800",
            "--rpc-secret=s3cret",
            "--dir=/tmp/dl",
            "--quiet=true",
            "--summary-interval=0",
        ] {
            assert!(
                args.iter().any(|a| a == expected),
                "缺少参数 {expected}：{args:?}"
            );
        }
        // 监听必须只在本机回环
        assert!(!args.iter().any(|a| a == "--rpc-listen-all=true"));
        // 父进程没了要跟着退（第二道保险）
        assert!(args.iter().any(|a| a.starts_with("--stop-with-process=")));
    }

    /// **不要用 `--split` / `--max-connection-per-server` 之外的名字猜**：
    /// 这两个在 `--help=#all` 里看不到，但通过 RPC 是存在的。
    /// 这条测试锁住"我们只用实测确认过的选项"，避免有人照着上游 aria2 的文档加参数。
    #[test]
    fn args_never_include_unverified_upstream_only_options() {
        let args = config().to_args(6800, "s");
        for bad in ["--check-certificate", "--bt-max-peers", "--seed-time"] {
            assert!(
                !args.iter().any(|a| a.starts_with(bad)),
                "不该出现未验证的选项 {bad}"
            );
        }
    }

    #[test]
    fn extra_args_are_appended_last_so_they_can_override() {
        let mut config = config();
        config.extra_args = vec!["--stream-max-connections=8".to_string()];
        let args = config.to_args(1, "s");
        assert_eq!(
            args.last().map(String::as_str),
            Some("--stream-max-connections=8"),
            "逃生舱参数必须排在最后（后出现的生效）"
        );
    }

    #[test]
    fn max_concurrent_falls_back_to_one() {
        let mut config = config();
        config.max_concurrent = 0;
        let args = config.to_args(1, "s");
        assert!(args.iter().any(|a| a == "--max-concurrent-downloads=1"));
    }

    /// 这几条与参考实现（`x-spider-mac` 的 `Aria2RPCClient.swift`）逐条对齐，
    /// 拆开看每一条都对应一个真实差异：
    /// - `--conf-path=/dev/null`：清掉用户机器上的 aria2 配置；
    /// - DHT/LPD/PEX 全关：这是 HTTP 下载器，不该在用户机器上开 UDP 端口
    ///   （会触发防火墙弹窗）；
    /// - `--continue` / `--auto-file-renaming=false` / `--allow-overwrite=true`：
    ///   断点续传 + 精确文件名。
    #[test]
    fn args_match_the_reference_implementation() {
        let args = config().to_args(6800, "s");
        for expected in [
            "--conf-path=/dev/null",
            "--enable-dht=false",
            "--enable-dht6=false",
            "--bt-enable-lpd=false",
            "--enable-peer-exchange=false",
            "--continue=true",
            "--auto-file-renaming=false",
            "--allow-overwrite=true",
        ] {
            assert!(
                args.iter().any(|a| a == expected),
                "缺少参数 {expected}：{args:?}"
            );
        }
    }

    /// 逐任务选项：UA / Referer / 多连接。
    ///
    /// - `user-agent` 与 `referer` 必须给（媒体 CDN 对着默认 UA 的态度与浏览器不同，
    ///   而参考实现两个都显式传了）；
    /// - 多连接的选项名是 **`stream-max-connections`**：上游 aria2 的 `--split` /
    ///   `--max-connection-per-server` 在 Aria2Next 里已退役，照抄旧名字等于没设。
    #[test]
    fn per_request_options_carry_ua_referer_and_connections() {
        let mut request = DownloadRequest::new("https://video.twimg.com/a.mp4", "/tmp/dl/a.mp4");
        request.segments = 6;
        let options = per_request_options(&request, Path::new("/tmp/dl"), "a.mp4.part");
        assert_eq!(
            options.get("user-agent").and_then(Value::as_str),
            Some(DOWNLOAD_USER_AGENT)
        );
        assert_eq!(
            options.get("referer").and_then(Value::as_str),
            Some(DOWNLOAD_REFERER)
        );
        assert_eq!(
            options
                .get("stream-max-connections")
                .and_then(Value::as_str),
            Some("6")
        );
        // 精确文件名与断点续传：引擎不许自作主张改名
        assert_eq!(
            options.get("auto-file-renaming").and_then(Value::as_str),
            Some("false")
        );
        assert_eq!(
            options.get("continue").and_then(Value::as_str),
            Some("true")
        );
    }

    /// `segments = 1`（默认）时**不要**给多连接选项：让引擎用自己的默认值，
    /// 而不是被我们钉成 1 连接。
    #[test]
    fn single_segment_does_not_pin_connections() {
        let request = DownloadRequest::new("https://a/b.mp4", "/tmp/dl/b.mp4");
        let options = per_request_options(&request, Path::new("/tmp/dl"), "b.mp4.part");
        assert!(options.get("stream-max-connections").is_none());
    }

    #[test]
    fn aria2_error_codes_map_to_structured_reasons() {
        // 3 = resource not found
        assert_eq!(map_aria2_error_code(3, "x"), DownloadError::NotFound);
        // 24 = HTTP authorization failed
        assert_eq!(
            map_aria2_error_code(24, "x"),
            DownloadError::AuthRequired { status: 403 }
        );
        // 8 = resume not supported → 传输类（可重试）
        assert!(matches!(
            map_aria2_error_code(8, "x"),
            DownloadError::Transport { .. }
        ));
        // 未知码不 panic、不猜语义，但要把数字码带出来
        match map_aria2_error_code(9999, "boom") {
            DownloadError::Transport { message, .. } => {
                assert!(message.contains("9999"), "{message}");
                assert!(message.contains("boom"), "{message}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn part_file_name_is_engine_specific() {
        let dest = PathBuf::from("/tmp/d/pic.jpg");
        let aria2_part = part_file_name(&dest, ATTRIBUTION_ARIA2);
        let http_part = crate::http_backend::part_path(&dest);
        assert_ne!(
            aria2_part, http_part,
            "两个引擎的断点名必须不同——否则换引擎会拿别人的半成品拼在一起（docs/02 §E3）"
        );
        assert!(aria2_part.contains("aria2"));
    }

    #[test]
    fn cleanup_removes_both_the_partial_and_the_control_file() {
        let dir = std::env::temp_dir().join(format!("xspider-a2-clean-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let part = dir.join("f.bin.part.aria2next");
        let control = with_aria2_suffix(&part);
        std::fs::write(&part, b"half").unwrap();
        std::fs::write(&control, b"ctrl").unwrap();

        cleanup_engine_files(&part);
        assert!(!part.exists(), ".part 必须被清掉");
        assert!(
            !control.exists(),
            ".aria2 控制文件也必须被清掉（docs/02 §E4）"
        );
        // 再清一次不能炸（幂等）
        cleanup_engine_files(&part);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn random_secret_is_not_empty_and_differs_each_time() {
        let a = random_secret();
        let b = random_secret();
        assert!(a.len() >= 20);
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn preflight_rejects_a_non_aria2next_binary() {
        // /bin/echo --version 会打印 "macOS ..."，显然不是 aria2-next
        let err = preflight(Path::new("/bin/echo")).await.unwrap_err();
        match err {
            DownloadError::Invalid { message } => {
                assert!(message.contains("Aria2Next"), "{message}");
            }
            other => panic!("期望 Invalid，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn preflight_reports_a_missing_binary_clearly() {
        let err = preflight(Path::new("/nonexistent/aria2next"))
            .await
            .unwrap_err();
        match err {
            DownloadError::Invalid { message } => {
                assert!(message.contains("XSPIDER_ARIA2_PATH"), "{message}");
            }
            other => panic!("期望 Invalid，实际 {other:?}"),
        }
    }

    #[test]
    fn locate_binary_finds_the_bundled_one_on_this_machine_when_present() {
        // 这台机器上应该有（x-spider-mac 随包带的那个）；没有也不算失败——
        // 但它一旦存在，就必须被找到（否则 E2E 会静默不跑）
        if bundled_candidates().iter().any(|p| p.is_file()) {
            assert!(Aria2NextConfig::locate_binary().is_some());
        }
    }

    /// 候选列表里**绝不能**出现写死的家目录：仓库是对外发布的，
    /// `/Users/<某人>/...` 只在他那台机器上有意义（而且不该出现在公开仓库里）。
    #[test]
    fn bundled_candidates_are_relative_not_a_personal_home_path() {
        for candidate in bundled_candidates() {
            let text = candidate.to_string_lossy();
            assert!(
                !text.contains("/Users/mac/"),
                "写死了家目录：{text}（要用 $HOME 或相对路径拼）"
            );
        }
    }
}
