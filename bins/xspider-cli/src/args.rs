//! 命令行参数。手写解析（与 `xspiderd` 同一风格）：能不加依赖就不加
//! （`docs/05-WORKFLOW.md` §4）。

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    /// 取谁的媒体。
    pub screen_name: String,
    /// 下载几个媒体（M4 的验收是 3 个）。
    pub count: usize,
    /// 落盘根目录。目录与文件名**由外壳算好**再交给组件（`docs/01` §4）。
    pub out: PathBuf,
    /// 每页条数。
    pub page_size: u64,
    /// 只要某一类媒体（`photo` / `video` / `animated_gif`）；缺省不限。
    pub media_kind: Option<String>,
    /// 只规划不下载（离线可跑：fixture 里的 URL 是假的）。
    pub dry_run: bool,
    /// sidecar 可执行文件；缺省找 `xspider-cli` 旁边的 `xspiderd`。
    pub sidecar: Option<PathBuf>,
    /// 回放 fixture 目录（离线，不联网）。
    pub fixture_dir: Option<PathBuf>,
    /// 代理 URL；缺省读 `XSPIDER_PROXY`。
    pub proxy: Option<String>,
    /// cookie；缺省读 `XSPIDER_COOKIE`。**只进不出**，不落盘、不打日志。
    pub cookie: Option<String>,
    /// 每个账号一个子目录（参考实现的 `accountSubfolderEnabled`）。
    pub subfolder_per_user: bool,
    /// 多连接分片数（`requirements.segments`）。
    pub segments: u8,
    /// 等下载完成的上限秒数。
    pub timeout_s: u64,
    /// 输出机器可读的 JSON 报告。
    pub json: bool,
    /// 打印 sidecar 的 info/debug 日志（默认压到 warn，只让告警和错误透出来）。
    pub verbose: bool,
    /// 只问组件支持哪些 method（外壳的启动自检）。
    pub list_methods: bool,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            screen_name: String::new(),
            count: 3,
            out: PathBuf::from("downloads"),
            page_size: 20,
            media_kind: None,
            dry_run: false,
            sidecar: None,
            fixture_dir: None,
            proxy: None,
            cookie: None,
            subfolder_per_user: false,
            segments: 1,
            timeout_s: 300,
            json: false,
            verbose: false,
            list_methods: false,
        }
    }
}

pub const USAGE: &str = "\
xspider-cli —— X-Spider 的第一个真实消费方（取一页 → 下 N 个媒体 → 报告结果）

用法：
  xspider-cli --screen-name <NAME> [选项]

必填：
  --screen-name <NAME>    要取的用户名（可带前导 @）

常用：
  --count <N>             下载几个媒体，默认 3
  --out <DIR>             落盘根目录，默认 ./downloads
  --dry-run               只取一页并列出计划，不下载（离线 fixture 也能跑）
  --json                  stdout 只输出一个 JSON 报告（进度走 stderr）
  --list-methods          只问组件支持哪些 method（外壳的启动自检），不取数

连接：
  --sidecar <PATH>        xspiderd 可执行文件；缺省找同目录下的 xspiderd，
                          再找 target/{debug,release}/xspiderd
  --fixture-dir <DIR>     回放 fixture，离线不联网（+ --dry-run 可在 CI 里跑）
  --proxy <URL>           代理；缺省读 XSPIDER_PROXY
  --cookie <STRING>       cookie；缺省读 XSPIDER_COOKIE（只进不出，不打日志）

细节：
  --page-size <N>         每页条数，默认 20
  --media-kind <KIND>     只要 photo / video / animated_gif
  --segments <N>          多连接分片数（>1 会派给外派后端），默认 1
  --subfolder-per-user    每个账号一个子目录（参考实现的 accountSubfolderEnabled）
  --timeout <SECONDS>     等下载完成的上限，默认 300
  --verbose               打印 sidecar 的 info 日志（默认只透出 warn 以上）
  -h, --help              显示本帮助
  -V, --version           显示版本

退出码：
  0  全部成功（dry-run 下 = 规划成功）
  1  有媒体没下成
  2  参数/用法错误
  3  契约或传输层失败（连不上 sidecar、组件报错、握手版本不匹配）

为什么这个二进制要单独存在：它是**外壳的替身**。它只经契约（JSON-RPC）使用组件，
不链接任何 xspider crate——契约缺字段、形状别扭，这里第一个炸。
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    Run(Box<Args>),
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
            "--dry-run" => args.dry_run = true,
            "--list-methods" => args.list_methods = true,
            "--json" => args.json = true,
            "--verbose" => args.verbose = true,
            "--subfolder-per-user" => args.subfolder_per_user = true,
            "--screen-name" => args.screen_name = next_value(&mut it, "--screen-name")?,
            "--media-kind" => {
                let kind = next_value(&mut it, "--media-kind")?;
                if !matches!(kind.as_str(), "photo" | "video" | "animated_gif") {
                    return Err(format!(
                        "--media-kind 只接受 photo / video / animated_gif（收到 {kind:?}）"
                    ));
                }
                args.media_kind = Some(kind);
            }
            "--out" => args.out = PathBuf::from(next_value(&mut it, "--out")?),
            "--sidecar" => args.sidecar = Some(PathBuf::from(next_value(&mut it, "--sidecar")?)),
            "--fixture-dir" => {
                args.fixture_dir = Some(PathBuf::from(next_value(&mut it, "--fixture-dir")?))
            }
            "--proxy" => args.proxy = Some(next_value(&mut it, "--proxy")?),
            "--cookie" => args.cookie = Some(next_value(&mut it, "--cookie")?),
            "--count" => args.count = parse_uint(&mut it, "--count")? as usize,
            "--page-size" => args.page_size = parse_uint(&mut it, "--page-size")?,
            "--segments" => args.segments = parse_uint(&mut it, "--segments")? as u8,
            "--timeout" => args.timeout_s = parse_uint(&mut it, "--timeout")?,
            other => {
                // 未知参数直接失败：拼错的开关会静默变成默认行为，那是最难查的一类
                return Err(format!("未知参数：{other}\n\n{USAGE}"));
            }
        }
    }

    if args.screen_name.trim().is_empty() && !args.list_methods {
        return Err(format!("必须给 --screen-name\n\n{USAGE}"));
    }
    if args.count == 0 && !args.dry_run {
        return Err("--count 至少是 1（只想看计划请用 --dry-run）".to_string());
    }
    if args.segments == 0 {
        return Err("--segments 至少是 1".to_string());
    }
    Ok(Parsed::Run(Box::new(args)))
}

fn parse_uint(
    it: &mut std::iter::Peekable<impl Iterator<Item = String>>,
    flag: &str,
) -> Result<u64, String> {
    let raw = next_value(it, flag)?;
    if raw.parse::<u64>().is_err() {
        return Err(format!("{flag} 必须是非负整数（收到 {raw:?}）"));
    }
    Ok(raw.parse().unwrap_or(0))
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
            Parsed::Run(a) => *a,
            other => panic!("期望 Run，实际 {other:?}"),
        }
    }

    #[test]
    fn defaults_are_the_m4_scenario() {
        let a = parse_ok(&["--screen-name", "jack"]);
        assert_eq!(a.count, 3, "M4 的验收就是下 3 个媒体");
        assert_eq!(a.page_size, 20);
        assert_eq!(a.segments, 1);
        assert!(!a.dry_run);
        assert!(!a.json);
        assert!(!a.subfolder_per_user);
    }

    #[test]
    fn every_documented_flag_parses() {
        let a = parse_ok(&[
            "--screen-name",
            "@jack",
            "--count",
            "5",
            "--out",
            "/tmp/o",
            "--page-size",
            "10",
            "--media-kind",
            "video",
            "--dry-run",
            "--sidecar",
            "/tmp/xspiderd",
            "--fixture-dir",
            "fixtures",
            "--proxy",
            "http://127.0.0.1:17890",
            "--cookie",
            "ct0=x",
            "--subfolder-per-user",
            "--segments",
            "4",
            "--timeout",
            "60",
            "--json",
            "--verbose",
        ]);
        assert_eq!(a.screen_name, "@jack");
        assert_eq!(a.count, 5);
        assert_eq!(a.out, PathBuf::from("/tmp/o"));
        assert_eq!(a.page_size, 10);
        assert_eq!(a.media_kind.as_deref(), Some("video"));
        assert!(a.dry_run && a.json && a.verbose && a.subfolder_per_user);
        assert_eq!(a.segments, 4);
        assert_eq!(a.timeout_s, 60);
    }

    #[test]
    fn missing_screen_name_fails_loudly() {
        assert!(parse(["--count".to_string(), "3".to_string()])
            .unwrap_err()
            .contains("--screen-name"));
    }

    #[test]
    fn bogus_values_fail_loudly() {
        assert!(parse([
            "--screen-name".to_string(),
            "a".to_string(),
            "--media-kind".to_string(),
            "mp3".to_string()
        ])
        .unwrap_err()
        .contains("photo"));
        assert!(parse([
            "--screen-name".to_string(),
            "a".to_string(),
            "--count".to_string(),
            "x".to_string()
        ])
        .unwrap_err()
        .contains("整数"));
        assert!(parse([
            "--screen-name".to_string(),
            "a".to_string(),
            "--count".to_string(),
            "0".to_string()
        ])
        .unwrap_err()
        .contains("至少是 1"));
        assert!(parse([
            "--screen-name".to_string(),
            "a".to_string(),
            "--segments".to_string(),
            "0".to_string()
        ])
        .unwrap_err()
        .contains("至少是 1"));
    }

    #[test]
    fn help_and_version_short_circuit() {
        assert_eq!(parse(["--help".to_string()]).unwrap(), Parsed::Help);
        assert_eq!(parse(["-V".to_string()]).unwrap(), Parsed::Version);
    }
}
