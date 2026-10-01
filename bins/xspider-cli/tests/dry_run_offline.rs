//! 端到端：**CLI 只经契约驱动组件**，离线（fixture 回放）跑通
//! 「取一页 → 规划 → 报告」这条链路。
//!
//! 为什么离线也要有这条测试：它是唯一一条"从**消费者**视角"看契约的测试。
//! 组件自己的测试可以用内部类型，这个 CLI 不行——它坏了通常意味着
//! **契约少了外壳需要的东西**，而不是 CLI 写错了。
//!
//! 真正的下载（真网络）不在这里：那需要凭据与代理，属于 live 冒烟。

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    // <repo>/bins/xspider-cli → <repo>
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("取仓库根目录")
        .to_path_buf()
}

fn fixtures() -> PathBuf {
    repo_root().join("fixtures")
}

/// 跑一次 CLI，返回 (退出码, stdout, stderr)。
fn run_cli(args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_xspider-cli"))
        .args(args)
        .env_remove("XSPIDER_COOKIE")
        .env_remove("XSPIDER_PROXY")
        .output()
        .expect("跑得起来 xspider-cli");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn parse_json(stdout: &str) -> serde_json::Value {
    serde_json::from_str(stdout)
        .unwrap_or_else(|e| panic!("stdout 不是 JSON（--json 下必须只有一个对象）：{e}\n{stdout}"))
}

/// 离线模式找不到 sidecar 时**大声失败**，不要静默跳过。
///
/// `cargo test --workspace` 会构建所有二进制，所以正常情况下它就在旁边。
/// 单独跑 `cargo test -p xspider-cli` 而没先构建过的话，这条会说清该做什么。
fn assert_sidecar_is_beside_the_cli() {
    let cli = PathBuf::from(env!("CARGO_BIN_EXE_xspider-cli"));
    let sidecar = cli.parent().expect("CLI 的目录").join("xspiderd");
    assert!(
        sidecar.is_file(),
        "找不到 {}——先 `cargo build -p xspiderd`，或用 `cargo test --workspace` 一次构建全部",
        sidecar.display()
    );
}

#[test]
fn dry_run_offline_plans_three_media_and_reports_json() {
    assert_sidecar_is_beside_the_cli();
    let out = std::env::temp_dir().join(format!("xspider-cli-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);

    let (code, stdout, stderr) = run_cli(&[
        "--screen-name",
        "demo_user",
        "--fixture-dir",
        fixtures().to_str().unwrap(),
        "--dry-run",
        "--json",
        "--out",
        out.to_str().unwrap(),
    ]);

    assert_eq!(code, 0, "dry-run 应当成功。stderr:\n{stderr}");
    let report = parse_json(&stdout);

    assert_eq!(report["ok"], true);
    assert_eq!(
        report["mode"], "replay",
        "得说清这是离线回放，别让人以为下了真东西"
    );
    assert_eq!(report["dry_run"], true);
    assert_eq!(report["contract_version"], "1.2.0");

    let plan = report["plan"].as_array().expect("plan 必须是数组");
    assert_eq!(plan.len(), 3, "fixture 那一页有 3 个媒体，--count 默认 3");

    for item in plan {
        assert_eq!(item["kind"], "video", "fixture 里是 3 条视频推文");
        assert!(item["url"].as_str().unwrap().starts_with("https://"));
        assert!(!item["job_id"].as_str().unwrap().is_empty());
        assert!(
            item["size"].is_null(),
            "离线回放不联网，net.probe_size 应当如实回答 null，而不是编一个数"
        );
        // 文件名规则来自参考实现的命名模板：时间 用户名 推文id-序号.扩展名
        let name = item["file_name"].as_str().unwrap();
        assert!(
            name.ends_with(".mp4") && name.contains("demo_user"),
            "文件名不符合约定：{name}"
        );
        assert_eq!(item["dest_dir"], out.to_string_lossy().as_ref());
    }

    // dry-run 不该入队、更不该落盘
    assert!(report["results"].as_array().unwrap().is_empty());
    assert_eq!(report["totals"]["planned"], 3);
    assert_eq!(report["totals"]["jobs"], 0);
    assert!(!out.exists(), "dry-run 不许写任何文件");
}

#[test]
fn media_kind_filter_that_matches_nothing_is_still_a_success() {
    assert_sidecar_is_beside_the_cli();
    let (code, stdout, _) = run_cli(&[
        "--screen-name",
        "demo_user",
        "--fixture-dir",
        fixtures().to_str().unwrap(),
        "--dry-run",
        "--json",
        "--media-kind",
        "photo",
    ]);
    assert_eq!(code, 0, "筛不到东西不是错误");
    let report = parse_json(&stdout);
    assert!(
        report["plan"].as_array().unwrap().is_empty(),
        "这一页没有图片"
    );
}

#[test]
fn usage_errors_exit_2_with_the_usage_text() {
    let (code, _, stderr) = run_cli(&["--nope"]);
    assert_eq!(
        code, 2,
        "用法错误用 2（与 1=有媒体没下成、3=契约失败区分开）"
    );
    assert!(stderr.contains("未知参数"), "{stderr}");

    let (code, _, stderr) = run_cli(&[]);
    assert_eq!(code, 2);
    assert!(stderr.contains("--screen-name"), "{stderr}");
}

#[test]
fn a_wrong_sidecar_path_is_a_transport_failure_not_a_silent_success() {
    let (code, _, stderr) = run_cli(&[
        "--screen-name",
        "demo_user",
        "--sidecar",
        "/nonexistent/xspiderd",
    ]);
    assert_eq!(code, 3, "连不上组件是契约/传输层失败");
    assert!(stderr.contains("失败"), "{stderr}");
}

/// 契约方法表的**消费方视角**：CLI 调用的每一个 method 都必须出现在组件公布的
/// `system.methods` 里。
///
/// 这条测试的价值在于"以契约为主"：CLI 里写死的 method 名与组件公布的清单
/// 一旦漂移，说明有人改了 method 名字——而契约只说"只增不改不删"。
#[test]
fn every_method_the_cli_calls_is_advertised_by_the_component() {
    assert_sidecar_is_beside_the_cli();

    // 1) 组件公布的清单（离线也能答：不问网络）
    let (code, stdout, stderr) = run_cli(&[
        "--list-methods",
        "--fixture-dir",
        fixtures().to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{stderr}");
    let advertised = parse_json(&stdout);
    let methods: Vec<String> = advertised["methods"]
        .as_array()
        .expect("system.methods 必须回 methods 数组")
        .iter()
        .map(|m| m.as_str().unwrap_or_default().to_string())
        .collect();
    assert!(!methods.is_empty());

    // 2) CLI 真的用到的那些（写在这里是刻意的：它是"第一个真实消费方"的最小集合）
    //
    // 注意这里**没有** `system.shutdown`：它按 `docs/CONTRACT.md` §3.2 属于
    // "sidecar 传输层 method"，**不属于契约载荷**，所以不出现在 `system.methods` 里
    // （cdylib 形态没有"关掉宿主进程"这回事）。这是一条真实的使用观察，
    // 记在 `docs/06-CONSUMER-INTEGRATION.md` 里。
    for method in [
        "system.version",
        "system.methods",
        "auth.set_cookie",
        "net.set_proxy",
        "net.probe_size",
        "fetch.get_user",
        "fetch.user_medias",
        "dl.enqueue",
        "dl.events",
        "dl.list",
        "dl.cancel",
    ] {
        assert!(
            methods.iter().any(|m| m == method),
            "CLI 要用 {method}，但组件没公布它。已公布：{methods:?}"
        );
    }

    // 3) 握手信息必须带齐（外壳靠它决定能不能用）
    assert!(advertised["contract_version"]
        .as_str()
        .unwrap()
        .starts_with('1'));
    assert!(advertised["build_version"].is_string());
    // 形态：走的是 HTTP sidecar，组件必须如实这么报——
    // 外壳据此知道 `system.shutdown` 可用（ADR-037）
    assert_eq!(
        advertised["transport"], "sidecar",
        "sidecar 形态必须自报 sidecar：{advertised}"
    );
}
