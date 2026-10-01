//! sidecar 客户端：起进程 → 读 ready 行 → 发 JSON-RPC。
//!
//! **这里只有契约里写过的东西**（`docs/CONTRACT.md` §2）：一个 `ready` 行、
//! 一个 `POST /`、两个包络键。任何需要"看一眼组件内部"才能写出来的代码，
//! 都说明契约漏了什么。

use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

/// 握手超时。冷启动实测在 1 秒内（`docs/03` §3），给足余量。
const READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// 契约错误（`error` 包络）。**只按 `code` 做判断**，`message` 只给人看。
#[derive(Debug, Clone)]
pub struct ContractError {
    pub code: String,
    pub message: String,
    pub endpoint: Option<String>,
    pub retry_after_s: Option<u64>,
    pub status: Option<u16>,
}

impl std::fmt::Display for ContractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}：{}", self.code, self.message)?;
        if let Some(endpoint) = &self.endpoint {
            write!(f, "（endpoint={endpoint}）")?;
        }
        if let Some(status) = self.status {
            write!(f, "（上游 HTTP {status}）")?;
        }
        if let Some(seconds) = self.retry_after_s {
            // 外壳拿它做倒计时；**不要**去猜 message 里的秒数
            write!(f, "（建议等 {seconds}s）")?;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum RpcError {
    /// 连不上 / 超时 / HTTP 层就错了。
    Transport(String),
    /// 组件按契约回了结构化错误。
    Contract(ContractError),
    /// 响应不是合法包络（版本不匹配，或者这不是我们的 sidecar）。
    Shape(String),
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RpcError::Transport(m) => write!(f, "传输层失败：{m}"),
            RpcError::Contract(e) => write!(f, "{e}"),
            RpcError::Shape(m) => write!(f, "响应形状不对：{m}"),
        }
    }
}

pub type RpcResult = Result<Value, RpcError>;

pub struct Sidecar {
    child: Child,
    port: u16,
    token: String,
    /// ready 行里的契约版本（外壳靠它握手）。
    pub contract_version: String,
    pub build_version: String,
    http: reqwest::Client,
    next_id: u64,
}

impl Sidecar {
    /// 起一个 sidecar 并完成握手。
    ///
    /// `login` 顺带把"这个二进制的路径"带出去，报错时能说清是哪个文件。
    pub async fn start(
        binary: &Path,
        fixture_dir: Option<&Path>,
        verbose: bool,
    ) -> Result<Self, RpcError> {
        let mut cmd = Command::new(binary);
        cmd.arg("--port").arg("0");
        if let Some(dir) = fixture_dir {
            cmd.arg("--fixture-dir").arg(dir);
        }
        // 日志走 stderr；默认压到 warn，免得 info 日志淹了 CLI 的输出。
        // 用户显式设了 XSPIDER_LOG 就尊重它。
        if std::env::var_os("XSPIDER_LOG").is_none() {
            cmd.env("XSPIDER_LOG", if verbose { "info" } else { "warn" });
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            // 兜底：我们自己崩了也不能留一个孤儿 sidecar 在后台
            .kill_on_drop(true);

        let mut child = cmd.spawn().map_err(|e| {
            RpcError::Transport(format!("起不来 sidecar（{}）：{e}", binary.display()))
        })?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| RpcError::Transport("读不到 sidecar 的 stdout".to_string()))?;
        let mut lines = BufReader::new(stdout).lines();

        let line = tokio::time::timeout(READY_TIMEOUT, lines.next_line())
            .await
            .map_err(|_| {
                RpcError::Transport(format!("{}s 内没等到 ready 行", READY_TIMEOUT.as_secs()))
            })?
            .map_err(|e| RpcError::Transport(format!("读 ready 行失败：{e}")))?
            .ok_or_else(|| {
                RpcError::Transport(
                    "sidecar 没打印 ready 行就退出了（看上面的 stderr）".to_string(),
                )
            })?;

        let payload = line
            .strip_prefix("ready ")
            .ok_or_else(|| RpcError::Shape(format!("stdout 第一行不是 ready 行：{line:?}")))?;
        let ready: Value = serde_json::from_str(payload)
            .map_err(|e| RpcError::Shape(format!("ready 行不是 JSON：{e}")))?;

        let port = ready
            .get("port")
            .and_then(Value::as_u64)
            .ok_or_else(|| RpcError::Shape("ready 行缺 port".to_string()))?
            as u16;
        let token = ready
            .get("token")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::Shape("ready 行缺 token".to_string()))?
            .to_string();
        let contract_version = ready
            .get("version")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::Shape("ready 行缺 version".to_string()))?
            .to_string();
        let build_version = ready
            .get("build")
            .and_then(Value::as_str)
            .unwrap_or("<未知>")
            .to_string();

        Ok(Self {
            child,
            port,
            token,
            contract_version,
            build_version,
            http: reqwest::Client::new(),
            next_id: 0,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// 调一个 method，成功时返回 `result` 的内容。
    pub async fn call(&mut self, method: &str, params: Value) -> RpcResult {
        self.next_id += 1;
        let body = json!({ "id": self.next_id, "method": method, "params": params });
        let response = self
            .http
            .post(format!("http://127.0.0.1:{}/", self.port))
            .header("X-XSpider-Token", &self.token)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| RpcError::Transport(format!("{method} 请求失败：{e}")))?;

        let status = response.status().as_u16();
        // 契约错误一律 200；非 200 只表达传输层发生了什么（§2.3）
        if status == 401 {
            return Err(RpcError::Transport(
                "401：token 不对（这不是契约错误，是传输层拒绝）".to_string(),
            ));
        }
        let text = response
            .text()
            .await
            .map_err(|e| RpcError::Transport(format!("读响应体失败：{e}")))?;
        let envelope: Value = serde_json::from_str(&text).map_err(|e| {
            RpcError::Shape(format!("HTTP {status} 的响应不是 JSON：{e}：{text:.200}"))
        })?;

        if let Some(error) = envelope.get("error") {
            return Err(RpcError::Contract(parse_error(error)));
        }
        envelope
            .get("result")
            .cloned()
            .ok_or_else(|| RpcError::Shape(format!("既没有 result 也没有 error：{text:.200}")))
    }

    /// 优雅关停：`system.shutdown` → 等退出 → 超时就强杀（不留残留进程）。
    pub async fn shutdown(&mut self) {
        let _ = self.call("system.shutdown", json!({})).await;
        match tokio::time::timeout(std::time::Duration::from_secs(5), self.child.wait()).await {
            Ok(_) => {}
            Err(_) => {
                let _ = self.child.kill().await;
                let _ = self.child.wait().await;
            }
        }
    }
}

fn parse_error(value: &Value) -> ContractError {
    ContractError {
        code: value
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("<缺 code>")
            .to_string(),
        message: value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        endpoint: value
            .get("endpoint")
            .and_then(Value::as_str)
            .map(str::to_string),
        retry_after_s: value.get("retry_after_s").and_then(Value::as_u64),
        status: value
            .get("status")
            .and_then(Value::as_u64)
            .map(|s| s as u16),
    }
}

/// 找 sidecar 可执行文件。
///
/// 顺序：显式路径 → 本二进制**旁边**的 `xspiderd`（`cargo run`/`cargo test`
/// 与打包产物都满足这条）→ 从当前目录往上找 `target/{debug,release}/xspiderd`
/// → `PATH`。
pub fn locate_sidecar(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return path.is_file().then(|| path.to_path_buf());
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("xspiderd"));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        for profile in ["debug", "release"] {
            candidates.push(cwd.join("target").join(profile).join("xspiderd"));
        }
    }
    if let Some(found) = candidates.into_iter().find(|p| p.is_file()) {
        return Some(found);
    }
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|dir| dir.join("xspiderd"))
        .find(|p| p.is_file())
}
