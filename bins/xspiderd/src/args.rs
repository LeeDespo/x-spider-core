//! 命令行参数。
//!
//! 刻意**不引入 clap**：这里只有 6 个开关，手写解析器更小、启动更快，
//! 而且能精确控制 `--help` 文案（`docs/05-WORKFLOW.md` §4：能不加依赖就不加）。

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    /// 监听端口。`0` = 让内核分配随机端口（默认，也是外壳应该用的方式）。
    pub port: u16,
    pub host: String,
    /// `--stdio`：改用 JSON Lines（每行一个请求）。备选形态。
    pub stdio: bool,
    /// 测试专用：从目录回放 fixture，**不联网**。
    pub fixture_dir: Option<PathBuf>,
    /// 实例状态目录。给了就启用单实例锁（多实例必须各自独立，见 docs/03 §3）。
    pub state_dir: Option<PathBuf>,
    /// 指定鉴权 token；缺省随机生成（**推荐留空**）。
    pub token: Option<String>,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            port: 0,
            host: "127.0.0.1".to_string(),
            stdio: false,
            fixture_dir: None,
            state_dir: None,
            token: None,
        }
    }
}

pub const USAGE: &str = "\
xspiderd —— X-Spider sidecar（把 xspider-ffi 的能力包成本地 JSON-RPC 服务）

用法：
  xspiderd [选项]

选项：
  --host <ADDR>         监听地址，默认 127.0.0.1（**不要**暴露到公网）
  --port <N>            监听端口；0 = 随机端口（默认）
  --stdio               改用 stdin/stdout 的 JSON Lines 协议（每行一个请求）
  --state-dir <DIR>     实例状态目录；**同时决定下载记录路径**（<DIR>/downloads.json）
                        与单实例锁。优先于 XSPIDER_STATE_DIR（没有它时锁也不启用）
  --fixture-dir <DIR>   【测试专用】从 fixture 回放，不发真实网络请求
  --token <TOKEN>       指定鉴权 token（默认随机生成；仅测试需要指定）
  -h, --help            显示本帮助
  -V, --version         显示版本

握手：
  `--port 0` 时会在 **stdout** 打印一行 `ready {\"port\":N,\"token\":\"...\",\"version\":\"...\"}`，
  外壳读这一行即完成握手。日志一律走 stderr，stdout 只用于这一行
  （`--stdio` 模式下 stdout 只走 JSON Lines，不再打印 ready 行）。

调试：
  curl -s localhost:<port> -H 'X-XSpider-Token: <token>' \\
       -d '{\"method\":\"fetch.get_user\",\"params\":{\"screen_name\":\"jack\"}}'
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    Run(Args),
    Help,
    Version,
}

pub fn parse(argv: impl IntoIterator<Item = String>) -> Result<Parsed, String> {
    let mut args = Args::default();
    let mut it = argv.into_iter().peekable();

    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "-V" | "--version" => return Ok(Parsed::Version),
            "--stdio" => args.stdio = true,
            "--host" => {
                args.host = next_value(&mut it, "--host")?;
            }
            "--port" => {
                let raw = next_value(&mut it, "--port")?;
                args.port = raw
                    .parse::<u16>()
                    .map_err(|e| format!("--port 必须是 0..65535 的整数（收到 {raw:?}）：{e}"))?;
            }
            "--state-dir" => {
                args.state_dir = Some(PathBuf::from(next_value(&mut it, "--state-dir")?));
            }
            "--fixture-dir" => {
                args.fixture_dir = Some(PathBuf::from(next_value(&mut it, "--fixture-dir")?));
            }
            "--token" => {
                args.token = Some(next_value(&mut it, "--token")?);
            }
            other => {
                // 未知参数直接失败，不要"忽略继续"——拼错的开关会静默变成默认行为
                return Err(format!("未知参数：{other}\n\n{USAGE}"));
            }
        }
    }

    if args.stdio && args.port != 0 {
        // 两种传输不能同时用；说出来比"忽略其中一个"安全
        return Err("--stdio 与 --port 不能同时使用".to_string());
    }
    Ok(Parsed::Run(args))
}

fn next_value(
    it: &mut std::iter::Peekable<impl Iterator<Item = String>>,
    flag: &str,
) -> Result<String, String> {
    it.next()
        .ok_or_else(|| format!("{flag} 需要跟一个值"))
        .and_then(|v| {
            if v.starts_with("--") {
                Err(format!("{flag} 缺少值（后面跟的是 {v}）"))
            } else {
                Ok(v)
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(args: &[&str]) -> Args {
        match parse(args.iter().map(|s| s.to_string())).unwrap() {
            Parsed::Run(a) => a,
            other => panic!("期望 Run，实际 {other:?}"),
        }
    }

    #[test]
    fn defaults_bind_a_random_local_port() {
        let a = parse_ok(&[]);
        assert_eq!(a.port, 0, "默认必须是随机端口：固定端口在多实例下必炸");
        assert_eq!(a.host, "127.0.0.1");
        assert!(!a.stdio);
        assert!(a.fixture_dir.is_none());
        assert!(a.token.is_none());
    }

    #[test]
    fn parses_every_documented_flag() {
        let a = parse_ok(&[
            "--host",
            "127.0.0.1",
            "--port",
            "0",
            "--fixture-dir",
            "/tmp/fx",
            "--state-dir",
            "/tmp/st",
            "--token",
            "t0ken",
            "--stdio",
        ]);
        // --stdio 与 --port 0 可以共存（端口本来就是默认值 0）
        assert!(a.stdio);
        assert_eq!(
            a.fixture_dir.as_deref(),
            Some(std::path::Path::new("/tmp/fx"))
        );
        assert_eq!(
            a.state_dir.as_deref(),
            Some(std::path::Path::new("/tmp/st"))
        );
        assert_eq!(a.token.as_deref(), Some("t0ken"));
    }

    #[test]
    fn help_and_version_short_circuit() {
        assert_eq!(parse(["--help".to_string()]).unwrap(), Parsed::Help);
        assert_eq!(parse(["-V".to_string()]).unwrap(), Parsed::Version);
    }

    #[test]
    fn unknown_flag_fails_loudly() {
        let err = parse(["--prot".to_string(), "1".to_string()]).unwrap_err();
        assert!(err.contains("未知参数"), "{err}");
    }

    #[test]
    fn missing_or_bogus_values_fail_loudly() {
        assert!(parse(["--port".to_string()])
            .unwrap_err()
            .contains("需要跟一个值"));
        assert!(parse(["--port".to_string(), "--stdio".to_string()])
            .unwrap_err()
            .contains("缺少值"));
        assert!(parse(["--port".to_string(), "abc".to_string()])
            .unwrap_err()
            .contains("整数"));
    }

    #[test]
    fn stdio_conflicts_with_an_explicit_port() {
        let err = parse([
            "--stdio".to_string(),
            "--port".to_string(),
            "1234".to_string(),
        ])
        .unwrap_err();
        assert!(err.contains("不能同时使用"), "{err}");
    }
}
