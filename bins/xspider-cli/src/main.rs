//! `xspider-cli` —— **第一个真实消费方**（M4，`docs/05-WORKFLOW.md` §2）。
//!
//! 它做的事就是 M4 的验收那句话：**取一页 → 下 3 个媒体 → 报告结果**。
//!
//! # 它为什么与别的二进制都不一样
//!
//! 它**不链接任何 `xspider-*` crate**，只经契约（本地 HTTP JSON-RPC）使用组件——
//! 也就是说，它是"别的地方写的外壳"的替身。契约里缺字段、形状别扭、
//! 少了某个必要信息，会在**这里**第一个炸，而不是被"反正 Rust 里能拿到"糊过去。
//! 守卫测试：`tests/only_the_contract.rs`。
//!
//! # 它顺带示范了两件外壳必须做的事
//!
//! 1. **握手与版本纪律**：`ready` 行里的契约版本与自己对不上就拒绝启动
//!    （`docs/CONTRACT.md` §6），而不是"降级成部分功能可用"；
//! 2. **目录与文件名由外壳算好**（`docs/01` §4）：命名逻辑留在 `plan.rs`，
//!    组件只收 `dest_dir` + `file_name`。

mod args;
mod plan;
mod sidecar;

use serde_json::{json, Value};

use crate::args::{Args, Parsed};
use crate::plan::{plan_from_page, Candidate, PageSummary};
use crate::sidecar::{locate_sidecar, RpcError, Sidecar};

/// 这个 CLI 是照着哪个契约版本写的。
///
/// 外壳的纪律（`docs/CONTRACT.md` §6）：启动握手，**主版本不同就拒绝启动**
/// 并给出可操作提示；次版本不同只提示（method 与字段只增不改不删）。
const WRITTEN_AGAINST: &str = "1.1.0";

fn main() {
    let parsed = match args::parse(std::env::args().skip(1)) {
        Ok(p) => p,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };

    match parsed {
        Parsed::Help => print!("{}", args::USAGE),
        Parsed::Version => println!(
            "xspider-cli {} (契约版本 {WRITTEN_AGAINST})",
            env!("CARGO_PKG_VERSION")
        ),
        Parsed::Run(args) => {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("构造 tokio 运行时失败：{e}");
                    std::process::exit(3);
                }
            };
            let code = runtime.block_on(run(*args));
            std::process::exit(code);
        }
    }
}

async fn run(args: Args) -> i32 {
    match run_inner(&args).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("失败：{e}");
            3
        }
    }
}

async fn run_inner(args: &Args) -> Result<i32, RpcError> {
    let sidecar_path = locate_sidecar(args.sidecar.as_deref()).ok_or_else(|| {
        RpcError::Transport(
            "找不到 xspiderd（用 --sidecar <PATH> 指定，或先 `cargo build -p xspiderd`）"
                .to_string(),
        )
    })?;

    let mut side = Sidecar::start(&sidecar_path, args.fixture_dir.as_deref(), args.verbose).await?;

    // ---- 握手：版本对不上就拒绝启动，别"降级成部分可用" ----
    if major(&side.contract_version) != major(WRITTEN_AGAINST) {
        eprintln!(
            "契约主版本不匹配：sidecar 是 {}，本 CLI 按 {WRITTEN_AGAINST} 写的。\n\
             拒绝继续（docs/CONTRACT.md §6）：请换一份匹配的 xspiderd。",
            side.contract_version
        );
        side.shutdown().await;
        return Ok(3);
    }
    if side.contract_version != WRITTEN_AGAINST {
        eprintln!(
            "注意：契约版本 {} 比本 CLI 按的 {WRITTEN_AGAINST} 新，按只增不改不删的规则应当兼容。",
            side.contract_version
        );
    }

    let result = if args.list_methods {
        list_methods(&mut side).await
    } else {
        drive(&mut side, args).await
    };
    side.shutdown().await; // 无论成败都优雅关停（不留残留进程）
    result
}

/// 外壳的启动自检：问组件"你支持哪些 method"。
///
/// 为什么值得做成一个开关：外壳要不要握手、能力够不够，
/// 是启动时第一个要回答的问题（`docs/CONTRACT.md` §6）。
async fn list_methods(side: &mut Sidecar) -> Result<i32, RpcError> {
    // 顺带把形态带出来：外壳靠它决定有没有 `system.shutdown`（ADR-037）
    let version = side.call("system.version", json!({})).await?;
    let result = side.call("system.methods", json!({})).await?;
    let methods = result
        .get("methods")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let report = json!({
        "contract_version": side.contract_version,
        "build_version": side.build_version,
        "transport": version.get("transport").cloned().unwrap_or(Value::Null),
        "count": methods.len(),
        "methods": methods,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&report).unwrap_or_default()
    );
    Ok(0)
}

async fn drive(side: &mut Sidecar, args: &Args) -> Result<i32, RpcError> {
    let offline = args.fixture_dir.is_some();
    let mut log = Reporter::new(args, side);

    // ---- 凭据：只进不出（不回显、不落盘；`--cookie` 的值不进任何日志） ----
    let cookie = args
        .cookie
        .clone()
        .or_else(|| std::env::var("XSPIDER_COOKIE").ok())
        .filter(|c| !c.trim().is_empty());
    match cookie {
        Some(cookie) => {
            side.call("auth.set_cookie", json!({ "cookie": cookie }))
                .await?;
        }
        None if offline => {
            // 回放模式下 fixture 分「带凭据 / 不带凭据」两种路由，离线跑需要走前者。
            // 这不是真凭据，只是让回放路由到 valid 分支。
            eprintln!("离线模式：没给 cookie，用占位值走回放（不会发任何网络请求）");
            side.call(
                "auth.set_cookie",
                json!({ "cookie": "ct0=offline-placeholder; auth_token=offline-placeholder" }),
            )
            .await?;
        }
        None => {
            eprintln!(
                "live 模式需要凭据。示例：\n\
                 \x20 export XSPIDER_COOKIE=\"$(defaults read moe.keli.xspider.mac app.cookieString)\"\n\
                 （只进不出：组件不打日志、不回传，本 CLI 也不写文件）"
            );
            return Ok(2);
        }
    }

    // ---- 代理：本机实测端口一天会变好几次，所以这个开关是必需品 ----
    let proxy = args
        .proxy
        .clone()
        .or_else(|| std::env::var("XSPIDER_PROXY").ok())
        .filter(|p| !p.trim().is_empty());
    if let Some(proxy) = proxy {
        side.call("net.set_proxy", json!({ "url": proxy })).await?;
    } else if !offline {
        eprintln!("提示：没设代理（--proxy / XSPIDER_PROXY）——直连不通时先怀疑这个");
    }

    // ---- 1. 取用户（外壳的第一次调用通常就是它：拿 user_id 才能取时间线） ----
    let user = call_with_retry(
        side,
        "fetch.get_user",
        json!({ "screen_name": args.screen_name }),
    )
    .await?
    .get("user")
    .cloned()
    .ok_or_else(|| RpcError::Shape("fetch.get_user 的结果里没有 user".to_string()))?;
    let user_id = user
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let screen_name = user
        .get("screen_name")
        .and_then(Value::as_str)
        .unwrap_or(&args.screen_name)
        .to_string();
    let user_name = user
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    log.user(&user, &user_id, &screen_name);

    // ---- 2. 取一页（**单页 + 游标**，不自动翻页：翻页时机是外壳的语义） ----
    let page = call_with_retry(
        side,
        "fetch.user_medias",
        json!({ "user_id": user_id, "count": args.page_size }),
    )
    .await?;

    // ---- 3. 规划：选哪几个、叫什么、放哪儿（全是外壳的活） ----
    let (mut candidates, summary) = plan_from_page(
        &page,
        &screen_name,
        &user_name,
        &args.out,
        args.subfolder_per_user,
        args.media_kind.as_deref(),
        args.count,
    )
    .map_err(RpcError::Shape)?;
    log.page(&summary, candidates.len());

    if candidates.is_empty() {
        eprintln!("这一页里没有符合条件的媒体（--media-kind 可能筛太严，或该页确实没有）");
        log.finish(&candidates, &[], 0);
        return Ok(0);
    }

    // ---- 4. 先问大小：外壳据此决定"要不要下 / 派给谁"（组件只回答大小） ----
    for candidate in &mut candidates {
        // 探测失败**不该让整轮失败**：大小是可缺的信息，真下不动的时候
        // 真正的错误会在下载那一步以结构化的形态出现（那里才有可操作的 code）。
        // 组件内部也是这个语义（探不到就按"未校验"继续）。
        match call_with_retry(side, "net.probe_size", json!({ "url": candidate.url })).await {
            Ok(probed) => candidate.size = probed.get("size").and_then(Value::as_u64),
            Err(e) => eprintln!("  探测大小失败（按未知处理）：{}", e),
        }
    }
    log.plan(&candidates);

    if args.dry_run {
        eprintln!("--dry-run：只规划，不入队、不下载");
        log.finish(&candidates, &[], 0);
        return Ok(0);
    }

    // ---- 5. 入队（`job_id` 由外壳生成，组件按它幂等） ----
    for candidate in &candidates {
        let mut params = json!({
            "job_id": candidate.job_id,
            "url": candidate.url,
            "dest_dir": candidate.dest_dir.to_string_lossy(),
            "file_name": candidate.file_name,
            "requirements": { "resume": true, "segments": args.segments },
            // `tag` 是**不透明**的：外壳放什么都行，组件只存不解释
            "tag": candidate.post_id,
        });
        if let Some(size) = candidate.size {
            params["expect_size"] = json!(size);
        }
        let accepted = call_with_retry(side, "dl.enqueue", params).await?;
        log.enqueued(candidate, &accepted);
    }

    // ---- 6. 等下载完成：事件流看进度，`dl.list` 决定"结束了没有" ----
    let results = wait_for_downloads(side, &candidates, args.timeout_s, &mut log).await?;

    let failures = results.iter().filter(|r| r.state != "complete").count();
    log.finish(&candidates, &results, failures);
    Ok(if failures == 0 { 0 } else { 1 })
}

/// 对**传输层失败**做有界重试（1s / 3s 退避）。
///
/// 组件内部对每次出网已经重试 3 次（150ms/450ms 退避，见 `crates/xspider-core`），
/// 但实测本机代理会出现**持续一两秒的整段拒连**（`AGENTS.md` 踩坑 8），
/// 那一小段预算不够。外壳——尤其是一次跑完就退出的 CLI——需要自己再兜一层。
///
/// 只重试 `transport`：契约错误（unauthorized / rate_limited / not_found / parse…）
/// 一律直接返回。这是"环境问题"与"代码问题"的分界，猜错方向会把
/// 一个"凭据失效"重试成一个更慢的"凭据失效"。
async fn call_with_retry(
    side: &mut Sidecar,
    method: &str,
    params: Value,
) -> Result<Value, RpcError> {
    const DELAYS: [u64; 2] = [1, 3];
    let mut attempt = 0usize;
    loop {
        match side.call(method, params.clone()).await {
            Ok(value) => return Ok(value),
            Err(RpcError::Transport(message)) => {
                eprintln!("  {method} 传输层失败（第 {} 次）：{message}", attempt + 1);
                let Some(delay) = DELAYS.get(attempt) else {
                    return Err(RpcError::Transport(message));
                };
                tokio::time::sleep(std::time::Duration::from_secs(*delay)).await;
                attempt += 1;
            }
            Err(other) => return Err(other),
        }
    }
}

/// 一个任务的终态。
#[derive(Debug, Clone)]
struct JobResult {
    job_id: String,
    state: String,
    done: u64,
    total: u64,
    reason: Option<String>,
    integrity: Option<String>,
    dest_path: Option<String>,
}

/// 轮询 `dl.events`（增量）＋ `dl.list`（权威状态）。
///
/// 为什么两个都用：事件是**增量日志**，用来画进度；而"结束了没有"必须问
/// `dl.list`/`dl.status`——外壳崩溃重连之后只有它是对的（`docs/01` §5.1）。
/// 这也是外壳接入时的真实写法。
async fn wait_for_downloads(
    side: &mut Sidecar,
    candidates: &[Candidate],
    timeout_s: u64,
    log: &mut Reporter<'_>,
) -> Result<Vec<JobResult>, RpcError> {
    let mut cursor = 0u64;
    let mut integrity: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut last_states: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_s);
    let wanted: Vec<&str> = candidates.iter().map(|c| c.job_id.as_str()).collect();

    loop {
        // 轮询期的传输抖动不该让整轮失败：等到 deadline 为止一直重试即可
        let events = match side.call("dl.events", json!({ "since": cursor })).await {
            Ok(events) => events,
            Err(RpcError::Transport(message)) => {
                eprintln!("  dl.events 传输层失败（继续轮询）：{message}");
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                continue;
            }
            Err(other) => return Err(other),
        };
        cursor = events.get("seq").and_then(Value::as_u64).unwrap_or(cursor);
        for event in events
            .get("events")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let kind = event.get("kind").and_then(Value::as_str).unwrap_or("");
            let job_id = event.get("job_id").and_then(Value::as_str).unwrap_or("");
            if !wanted.contains(&job_id) {
                continue; // 队列可能还留着上次跑的任务
            }
            match kind {
                "completed" => {
                    let label = integrity_label(event.get("integrity"));
                    integrity.insert(job_id.to_string(), label.clone());
                    log.completed(job_id, event.get("bytes").and_then(Value::as_u64), &label);
                }
                "failed" => {
                    let reason = event.get("reason").and_then(Value::as_str).unwrap_or("?");
                    log.failed(job_id, reason, event.get("error"));
                }
                "skipped" => {
                    let reason = event.get("reason").and_then(Value::as_str).unwrap_or("?");
                    log.skipped(job_id, reason);
                }
                "progress" => {
                    let done = event.get("done").and_then(Value::as_u64).unwrap_or(0);
                    let total = event.get("total").and_then(Value::as_u64).unwrap_or(0);
                    log.progress(job_id, done, total);
                }
                _ => {}
            }
        }

        // 状态：以 `dl.list` 为准
        let list = match side.call("dl.list", json!({})).await {
            Ok(list) => list,
            Err(RpcError::Transport(message)) => {
                eprintln!("  dl.list 传输层失败（继续轮询）：{message}");
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                continue;
            }
            Err(other) => return Err(other),
        };
        let jobs = list
            .get("jobs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut results = Vec::new();
        for job in &jobs {
            let job_id = job.get("job_id").and_then(Value::as_str).unwrap_or("");
            if !wanted.contains(&job_id) {
                continue;
            }
            let state = job
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            if last_states.get(job_id) != Some(&state) {
                log.state_change(job_id, &state);
                last_states.insert(job_id.to_string(), state.clone());
            }
            results.push(JobResult {
                job_id: job_id.to_string(),
                state,
                done: job.get("done").and_then(Value::as_u64).unwrap_or(0),
                total: job.get("total").and_then(Value::as_u64).unwrap_or(0),
                reason: job
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                integrity: integrity.get(job_id).cloned(),
                dest_path: job
                    .get("dest_path")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            });
        }

        let all_terminal = !results.is_empty()
            && results
                .iter()
                .all(|r| matches!(r.state.as_str(), "complete" | "error"));
        if all_terminal {
            return Ok(results);
        }

        if std::time::Instant::now() >= deadline {
            eprintln!(
                "等超时（{}s）：把还在跑的任务取消掉，再报告当前状态",
                timeout_s
            );
            for job_id in &wanted {
                let _ = side.call("dl.cancel", json!({ "job_id": job_id })).await;
            }
            return Ok(results);
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

/// 完整性结论的短标签。**只看结构化字段**（`{"result": ...}`），不猜文案。
fn integrity_label(value: Option<&Value>) -> String {
    let Some(value) = value else {
        return "未校验".to_string();
    };
    match value.get("result").and_then(Value::as_str) {
        Some("verified") => format!(
            "已校验 {}/{}",
            value.get("actual").and_then(Value::as_u64).unwrap_or(0),
            value.get("expected").and_then(Value::as_u64).unwrap_or(0)
        ),
        Some("unverified") => "未校验（服务端没给大小）".to_string(),
        _ => "未知".to_string(),
    }
}

fn major(version: &str) -> &str {
    version.split('.').next().unwrap_or(version)
}

// ---------------------------------------------------------------------------
// 报告：人读的走 stdout（`--json` 时 stdout 只有那一个 JSON，进度走 stderr）
// ---------------------------------------------------------------------------

struct Reporter<'a> {
    args: &'a Args,
    contract: String,
    build: String,
    pid: Option<u32>,
    port: u16,
    started: std::time::Instant,
    human: bool,
    /// 每个任务上次报过的进度百分比。
    ///
    /// **节流是调用方的事**（契约原话）：组件按"每片都报"发事件，
    /// 外壳要自己决定多久刷新一次界面。250ms 轮询 + 不节流 =
    /// 一个 68 MiB 的下载能刷出几十行一模一样的进度。
    last_percent: std::cell::RefCell<std::collections::HashMap<String, u32>>,
}

impl<'a> Reporter<'a> {
    fn new(args: &'a Args, side: &Sidecar) -> Self {
        Self {
            args,
            contract: side.contract_version.clone(),
            build: side.build_version.clone(),
            pid: side.pid(),
            port: side.port(),
            started: std::time::Instant::now(),
            human: !args.json,
            last_percent: std::cell::RefCell::new(std::collections::HashMap::new()),
        }
    }

    fn out(&self, line: &str) {
        if self.human {
            println!("{line}");
        }
    }

    /// 进度类输出一律走 stderr：`--json` 时 stdout 必须只有一个 JSON 对象。
    fn note(&self, line: &str) {
        eprintln!("{line}");
    }

    fn header(&self) {
        self.out(&format!(
            "X-Spider CLI · 契约 {} / 构建 {} · sidecar pid={} port={}",
            self.contract,
            self.build,
            self.pid
                .map(|p| p.to_string())
                .unwrap_or_else(|| "?".into()),
            self.port
        ));
        if self.args.fixture_dir.is_some() {
            self.out("模式：离线（fixture 回放，不发真实网络请求）");
        }
    }

    fn user(&self, user: &Value, user_id: &str, screen_name: &str) {
        self.header();
        let name = user.get("name").and_then(Value::as_str).unwrap_or("");
        let media_count = match user.get("media_count") {
            Some(Value::Null) | None => "未知".to_string(),
            Some(v) => v.to_string(),
        };
        self.out(&format!(
            "用户：{screen_name}（{name}）id={user_id} 媒体数={media_count}"
        ));
    }

    fn page(&self, summary: &PageSummary, planned: usize) {
        self.out(&format!(
            "取到一页：{} 条推文 / {} 个媒体 · {} · 计划 {} 个",
            summary.posts,
            summary.medias,
            if summary.says_more() {
                "服务端还有更多（游标可续）"
            } else {
                "服务端到底了"
            },
            planned
        ));
    }

    fn plan(&self, candidates: &[Candidate]) {
        let known = candidates.iter().filter(|c| c.size.is_some()).count();
        let unknown = candidates.len() - known;
        if known == 0 {
            self.out(&format!(
                "大小：{unknown} 个都未知（离线回放不联网，或服务端没给 Content-Length）"
            ));
        } else {
            let total: u64 = candidates.iter().filter_map(|c| c.size).sum();
            self.out(&format!(
                "大小合计 {}（{known} 个已知 / {unknown} 个未知）",
                human_size(total)
            ));
        }
        for candidate in candidates {
            self.out(&format!(
                "  {:<12} {:>9}  {:<12} {}  → {}",
                candidate.kind,
                candidate
                    .size
                    .map(human_size)
                    .unwrap_or_else(|| "未知".to_string()),
                candidate.job_id,
                candidate.post_time,
                candidate.dest_dir.join(&candidate.file_name).display()
            ));
        }
    }

    fn enqueued(&self, candidate: &Candidate, accepted: &Value) {
        let by = accepted
            .get("accepted_by")
            .and_then(Value::as_str)
            .unwrap_or("?");
        self.note(&format!("入队 {} → {by}", candidate.job_id));
    }

    fn state_change(&self, job_id: &str, state: &str) {
        self.note(&format!("  [{job_id}] {state}"));
    }

    fn progress(&self, job_id: &str, done: u64, total: u64) {
        if total == 0 || done >= total {
            return;
        }
        let percent = (done * 100 / total) as u32;
        {
            let mut last = self.last_percent.borrow_mut();
            let previous = last.get(job_id).copied().unwrap_or(0);
            // 每 5 个百分点报一次；99% 之后不再刷（最后那段经常卡在同一个数字上）
            if percent < previous + 5 || previous >= 99 {
                return;
            }
            last.insert(job_id.to_string(), percent);
        }
        self.note(&format!(
            "  [{job_id}] {percent}% ({}/{})",
            human_size(done),
            human_size(total)
        ));
    }

    fn completed(&self, job_id: &str, bytes: Option<u64>, integrity: &str) {
        self.note(&format!(
            "  [{job_id}] 完成 {} · {integrity}",
            bytes.map(human_size).unwrap_or_else(|| "?".to_string())
        ));
    }

    fn failed(&self, job_id: &str, reason: &str, error: Option<&Value>) {
        let code = error
            .and_then(|e| e.get("code"))
            .and_then(Value::as_str)
            .unwrap_or("?");
        self.note(&format!("  [{job_id}] 失败 reason={reason} code={code}"));
    }

    fn skipped(&self, job_id: &str, reason: &str) {
        self.note(&format!("  [{job_id}] 跳过 reason={reason}"));
    }

    /// 收尾：`--json` 时输出机器可读报告（含计划，dry-run 下就只有计划）。
    fn finish(&self, candidates: &[Candidate], results: &[JobResult], failures: usize) {
        let elapsed_ms = self.started.elapsed().as_millis() as u64;
        let bytes: u64 = results
            .iter()
            .filter(|r| r.state == "complete")
            .map(|r| r.done)
            .sum();
        if self.args.json {
            let report = json!({
                "ok": failures == 0,
                "contract_version": self.contract,
                "build_version": self.build,
                "mode": if self.args.fixture_dir.is_some() { "replay" } else { "live" },
                "dry_run": self.args.dry_run,
                "plan": candidates.iter().map(|c| json!({
                    "job_id": c.job_id,
                    "post_id": c.post_id,
                    "media_id": c.media_id,
                    "screen_name": c.screen_name,
                    "kind": c.kind,
                    "url": c.url,
                    "size": c.size,
                    "file_name": c.file_name,
                    "dest_dir": c.dest_dir.to_string_lossy(),
                    "post_time": c.post_time,
                    "media_index": c.media_index,
                })).collect::<Vec<_>>(),
                "results": results.iter().map(|r| json!({
                    "job_id": r.job_id,
                    "state": r.state,
                    "bytes": r.done,
                    "total": r.total,
                    "reason": r.reason,
                    "integrity": r.integrity,
                    "dest_path": r.dest_path,
                })).collect::<Vec<_>>(),
                "totals": {
                    "planned": candidates.len(),
                    "jobs": results.len(),
                    "complete": results.iter().filter(|r| r.state == "complete").count(),
                    "failed": failures,
                    "bytes": bytes,
                    "elapsed_ms": elapsed_ms,
                },
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&report).unwrap_or_default()
            );
            return;
        }
        if results.is_empty() {
            println!("（dry-run：没有入队任何任务）");
            return;
        }
        println!(
            "下载：{}/{} 成功 · {} 失败 · 落盘 {} · 用时 {:.1}s",
            results.iter().filter(|r| r.state == "complete").count(),
            results.len(),
            failures,
            human_size(bytes),
            elapsed_ms as f64 / 1000.0
        );
    }
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_size_is_readable() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(1024), "1.0 KiB");
        assert_eq!(human_size(3_122_633), "3.0 MiB");
    }

    #[test]
    fn integrity_label_reads_the_structured_field() {
        assert_eq!(
            integrity_label(Some(
                &json!({"result":"verified","expected":10,"actual":10})
            )),
            "已校验 10/10"
        );
        assert_eq!(
            integrity_label(Some(&json!({"result":"unverified","actual":10}))),
            "未校验（服务端没给大小）"
        );
        assert_eq!(integrity_label(None), "未校验");
    }

    #[test]
    fn version_major_is_compared_not_the_whole_string() {
        assert_eq!(major("1.1.0"), "1");
        assert_eq!(major("2.0.0"), "2");
        assert_eq!(major("weird"), "weird");
    }
}
