//! **双形态契约测试**：同一组用例分别打到库形态与 sidecar 形态，断言行为一致。
//!
//! `docs/04-TESTING-AND-FIXTURES.md` §4 原文：「不一致即缺陷（不是"次要偏差"）」。
//!
//! 一致性其实是**结构性**的——`xspiderd` 依赖 `xspider-ffi` 的 rlib 并直接调用
//! 同一个 `Engine::call`（`docs/01-ARCHITECTURE.md` §8：「合并的是部署，不是设计」）。
//! 这个测试的价值在于**把这件事钉住**：哪天有人"为了 sidecar 方便"在旁边另写一条
//! 分支，它会立刻红。
//!
//! 全离线：`--fixture-dir` / `XSPIDER_FIXTURE_DIR` 让两种形态都回放录制下来的
//! 真实响应，不碰网络。

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::Value;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

/// 一个用例 = 一次调用。两种形态跑同一串用例、同一个顺序
/// （顺序也重要：`auth.set_cookie` 必须排在 `fetch.get_user` 之前）。
struct Case {
    name: &'static str,
    method: &'static str,
    params: &'static str,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "握手：契约版本",
            method: "system.version",
            params: "{}",
        },
        Case {
            name: "自省：method 列表",
            method: "system.methods",
            params: "{}",
        },
        Case {
            name: "限流状态（初始）",
            method: "net.status",
            params: "{}",
        },
        // 先测"没有凭据"的分支——此时还没调 set_cookie
        Case {
            name: "未注入凭据就取数",
            method: "fetch.get_user",
            params: r#"{"screen_name":"demo_user"}"#,
        },
        Case {
            name: "未知 method",
            method: "fetch.nope",
            params: "{}",
        },
        Case {
            name: "params 不是对象",
            method: "fetch.get_user",
            params: "[]",
        },
        Case {
            name: "缺必填字段",
            method: "fetch.get_user",
            params: "{}",
        },
        Case {
            name: "字段类型不对",
            method: "fetch.get_user",
            params: r#"{"screen_name":42}"#,
        },
        Case {
            name: "net.set_limits 缺字段",
            method: "net.set_limits",
            params: r#"{"api_rps":2.0,"api_burst":4,"cdn_concurrency":4}"#,
        },
        Case {
            name: "net.set_limits 正常",
            method: "net.set_limits",
            params: r#"{"api_rps":2.0,"api_burst":4,"cdn_concurrency":4,"cooldown_s":120}"#,
        },
        Case {
            name: "net.set_proxy 关闭",
            method: "net.set_proxy",
            params: r#"{"url":null}"#,
        },
        Case {
            name: "net.set_proxy 漏 url 字段",
            method: "net.set_proxy",
            params: "{}",
        },
        Case {
            name: "注入凭据",
            method: "auth.set_cookie",
            params: r#"{"cookie":"auth_token=fixture; ct0=fixture"}"#,
        },
        Case {
            name: "凭据里没有可推导的 csrf",
            method: "auth.set_cookie",
            params: r#"{"cookie":"auth_token=fixture"}"#,
        },
        // 到此凭据已注入 → 命中 normal fixture
        Case {
            name: "取用户（真实响应回放）",
            method: "fetch.get_user",
            params: r#"{"screen_name":"demo_user"}"#,
        },
        Case {
            name: "取用户：@ 前缀与空白会被归一化",
            method: "fetch.get_user",
            params: r#"{"screen_name":"  @demo_user  "}"#,
        },
        Case {
            name: "取用户：不存在 → not_found",
            method: "fetch.get_user",
            params: r#"{"screen_name":"a_user_that_does_not_exist"}"#,
        },
        Case {
            name: "取用户：空用户名",
            method: "fetch.get_user",
            params: r#"{"screen_name":"   "}"#,
        },
        Case {
            name: "限流状态（跑过之后）",
            method: "net.status",
            params: "{}",
        },
        // 下载与爬取：这里只用"确定性且不需要网络"的两条，
        // 行为本身由 crates/xspider-download 的 E2E 覆盖。
        // 它们要证明的是**双形态走的是同一条派发路径**。
        Case {
            name: "爬取：source 不合法",
            method: "crawl.run",
            params: r#"{"source":"nope","user_id":"1"}"#,
        },
        Case {
            name: "下载：未知任务",
            method: "dl.status",
            params: r#"{"job_id":"no-such-job"}"#,
        },
        Case {
            name: "下载：入队缺字段",
            method: "dl.enqueue",
            params: r#"{"job_id":"a","url":"http://x/y"}"#,
        },
        Case {
            name: "下载：file_name 里混了路径",
            method: "dl.enqueue",
            params: r#"{"job_id":"a","url":"http://x/y","dest_dir":"/tmp","file_name":"a/b.jpg"}"#,
        },
    ]
}

/// 库形态：走真正的 C ABI（含 `catch_unwind`、CString 所有权、字符串化）——
/// 只测 Rust API 会漏掉整整一层。
fn library_form() -> Vec<Value> {
    // 必须在**第一次**构建引擎之前设置：引擎是进程级单例（OnceLock）
    std::env::set_var("XSPIDER_FIXTURE_DIR", fixtures_dir());
    std::env::remove_var("XSPIDER_PROXY");

    cases()
        .iter()
        .map(|case| {
            let raw = xspider::call_as_json(case.method, case.params);
            serde_json::from_str(&raw)
                .unwrap_or_else(|e| panic!("[{}] 库形态返回的不是合法 JSON：{e}\n{raw}", case.name))
        })
        .collect()
}

/// sidecar 形态：起真进程 → 读 ready 行 → 逐个用例发 HTTP。
fn sidecar_form() -> (Vec<Value>, Value) {
    let child = SidecarProcess::spawn();
    let ready = child.ready.clone();
    let port = ready["port"].as_u64().expect("ready 行里没有 port") as u16;
    let token = ready["token"]
        .as_str()
        .expect("ready 行里没有 token")
        .to_string();

    let responses = cases()
        .iter()
        .map(|case| {
            let body = serde_json::json!({ "method": case.method, "params": serde_json::from_str::<Value>(case.params).unwrap() })
                .to_string();
            let (status, value) = http_post(port, &token, &body);
            assert_eq!(
                status, 200,
                "[{}] 契约错误必须用 200 表达，实际 HTTP {status}：{value}",
                case.name
            );
            value
        })
        .collect();

    (responses, ready)
}

struct SidecarProcess {
    child: Child,
    ready: Value,
}

impl SidecarProcess {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_xspiderd"))
            .arg("--port")
            .arg("0")
            .arg("--fixture-dir")
            .arg(fixtures_dir())
            // stderr 继承：失败时能直接看到 sidecar 的日志，比"静默失败"好
            .stderr(Stdio::inherit())
            .stdout(Stdio::piped())
            .spawn()
            .expect("启动 xspiderd 失败");

        let stdout = child.stdout.take().expect("没有拿到 stdout");
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        // 超时保护：如果 sidecar 没打印 ready 行，这里会阻塞
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            line.clear();
            let read = reader.read_line(&mut line).expect("读 ready 行失败");
            if read > 0 && line.starts_with("ready ") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "20s 内没有等到 ready 行（stdout 只应打印 ready 行，日志必须走 stderr）"
            );
        }
        let payload = line.trim_start_matches("ready ").trim();
        let ready: Value = serde_json::from_str(payload)
            .unwrap_or_else(|e| panic!("ready 行不是合法 JSON：{e}\n{payload}"));

        // 把 reader 继续留在子进程的 stdout 上（不要关闭管道，否则 sidecar 写日志会出错）
        std::mem::forget(reader);

        Self { child, ready }
    }
}

impl Drop for SidecarProcess {
    fn drop(&mut self) {
        // SIGTERM → 走优雅退出路径；超时则硬杀，避免测试留下僵尸进程
        unsafe {
            libc_free_kill(self.child.id());
        }
        for _ in 0..50 {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                Err(_) => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// 用系统 `kill` 发 SIGTERM：不引入 libc 依赖（M0 不为一个测试加 crate）。
#[cfg(unix)]
#[allow(non_snake_case)]
unsafe fn libc_free_kill(pid: u32) {
    let _ = Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status();
}

#[cfg(not(unix))]
unsafe fn libc_free_kill(_pid: u32) {}

/// 最小 HTTP/1.1 客户端：刻意**不用 reqwest**。
/// 这样这个测试同时验证了"sidecar 说的是标准 HTTP，curl 能直接调"这件事。
fn http_post(port: u16, token: &str, body: &str) -> (u16, Value) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("连接 sidecar 失败");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("设置读超时失败");
    let request = format!(
        "POST / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\
         X-XSpider-Token: {token}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).expect("写请求失败");
    stream.flush().ok();

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("读响应失败");
    let text = String::from_utf8_lossy(&raw).replace('\r', "").to_string();
    let (head, rest) = text.split_once("\n\n").expect("响应缺少头体分隔");
    let status: u16 = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("响应状态行无法解析：{head}"));

    let body_text = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        decode_chunked(rest)
    } else {
        rest.to_string()
    };
    let value = serde_json::from_str(body_text.trim())
        .unwrap_or_else(|e| panic!("响应体不是合法 JSON：{e}\n{body_text}"));
    (status, value)
}

/// axum 对已知长度的响应会给 Content-Length，但别赌它。
fn decode_chunked(raw: &str) -> String {
    let mut out = String::new();
    let mut rest = raw;
    while let Some((size_line, after)) = rest.split_once('\n') {
        let size = usize::from_str_radix(size_line.trim().split(';').next().unwrap_or("0"), 16)
            .unwrap_or(0);
        if size == 0 {
            break;
        }
        if after.len() < size {
            out.push_str(after);
            break;
        }
        out.push_str(&after[..size]);
        rest = after.get(size + 1..).unwrap_or("");
    }
    out
}

/// 去掉 `transport` 字段后的副本（只给 `system.version` 的双形态比较用）。
fn without_transport(mut value: Value) -> Value {
    if let Some(result) = value.get_mut("result").and_then(Value::as_object_mut) {
        result.remove("transport");
    }
    value
}

#[test]
fn contract_dual_end_to_end() {
    let library = library_form();
    let (sidecar, ready) = sidecar_form();

    assert_eq!(
        library.len(),
        sidecar.len(),
        "两种形态的用例数量不一致（这本身说明用例表被改坏了）"
    );

    // 握手一致性：ready 行里的版本必须等于 xspider_version()
    assert_eq!(
        ready["version"].as_str(),
        Some(xspider_core::CONTRACT_VERSION),
        "ready 行的版本与 xspider_version() 不一致"
    );

    let mut mismatches = Vec::new();
    for (i, case) in cases().iter().enumerate() {
        // **一个例外，而且是刻意的**：`system.version` 的 `transport` 字段
        // 报的就是"我在哪种形态里"，两种形态本来就该不同（ADR-037）。
        // 写成显式的例外而不是"跳过这个用例"：例外一旦能被静默添加，
        // 这个测试就慢慢变成了摆设。
        let (a, b) = if case.method == "system.version" {
            (
                without_transport(library[i].clone()),
                without_transport(sidecar[i].clone()),
            )
        } else {
            (library[i].clone(), sidecar[i].clone())
        };
        if a != b {
            mismatches.push(format!(
                "[{}] method={}\n  库形态：{}\n  sidecar：{}",
                case.name,
                case.method,
                serde_json::to_string(&library[i]).unwrap(),
                serde_json::to_string(&sidecar[i]).unwrap()
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "双形态行为不一致（`docs/04` §4：不一致即缺陷）：\n{}",
        mismatches.join("\n")
    );

    // 反向自检：用例表不能全是错误包络——否则"一致"毫无意义
    let ok_count = library.iter().filter(|v| v.get("result").is_some()).count();
    let err_count = library.iter().filter(|v| v.get("error").is_some()).count();
    assert_eq!(
        ok_count + err_count,
        library.len(),
        "出现了既非 result 也非 error 的响应"
    );
    assert!(ok_count >= 8, "成功用例太少（{ok_count}），覆盖度不足");
    assert!(err_count >= 6, "错误用例太少（{err_count}），覆盖度不足");

    // 进程级单例（Engine 的 OnceLock）不允许被并行测试互相污染，
    // 所以下面这些自检在**同一个测试内**顺序执行，顺序即用例表里那条隐式状态机。
    check_fixtures_are_actually_exercised();
    check_unknown_method_never_panics_or_returns_empty();
    check_methods_are_reported_consistently();
}

/// 断言真实 fixture 确实被用上了（不是"因为两边都失败所以一致"）。
fn check_fixtures_are_actually_exercised() {
    std::env::set_var("XSPIDER_FIXTURE_DIR", fixtures_dir());
    std::env::remove_var("XSPIDER_PROXY");

    let call = |method: &str, params: &str| -> Value {
        let raw = xspider::call_as_json(method, params);
        serde_json::from_str(&raw).unwrap()
    };

    call(
        "auth.set_cookie",
        r#"{"cookie":"auth_token=fixture; ct0=fixture"}"#,
    );

    let ok = call("fetch.get_user", r#"{"screen_name":"demo_user"}"#);
    let user = &ok["result"]["user"];
    assert_eq!(user["screen_name"], "demo_user", "真实响应回放没生效：{ok}");
    assert!(
        user["register_time"].is_string(),
        "真实响应里的 created_at 应被归一化成 RFC3339：{ok}"
    );
    assert!(
        user["avatar"].as_str().unwrap().starts_with("https://"),
        "{ok}"
    );

    let missing = call(
        "fetch.get_user",
        r#"{"screen_name":"a_user_that_does_not_exist"}"#,
    );
    assert_eq!(
        missing["error"]["code"], "not_found",
        "真实 not_found 响应（HTTP 200 + data:{{}}）必须映射成 not_found：{missing}"
    );
}

/// 未知 method 必须是**结构化错误**，不能 panic、不能返回空字符串（docs/04 §4）。
fn check_unknown_method_never_panics_or_returns_empty() {
    std::env::set_var("XSPIDER_FIXTURE_DIR", fixtures_dir());
    for method in [
        "",
        "fetch.",
        "system.version.extra",
        "FETCH.GET_USER",
        "auth.set_cookie",
    ] {
        let raw = xspider::call_as_json(method, "{}");
        assert!(!raw.trim().is_empty(), "method={method:?} 返回了空字符串");
        let value: Value = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("method={method:?} 返回的不是 JSON：{e}\n{raw}"));
        let has_result = value.get("result").is_some();
        let has_error = value.get("error").is_some();
        assert!(
            has_result || has_error,
            "method={method:?} 的响应既没有 result 也没有 error：{value}"
        );
        if has_error {
            let code = value["error"]["code"].as_str().unwrap_or("");
            // 只有已知 method 才允许成功；其余必须是 invalid_request
            assert_eq!(code, "invalid_request", "method={method:?} → {value}");
        }
    }
}

/// 双形态里 `system.methods` 报出来的集合必须一样，且等于代码里登记的集合。
fn check_methods_are_reported_consistently() {
    std::env::set_var("XSPIDER_FIXTURE_DIR", fixtures_dir());
    let value: Value =
        serde_json::from_str(&xspider::call_as_json("system.methods", "{}")).unwrap();
    let reported: BTreeSet<String> = value["result"]["methods"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    let expected: BTreeSet<String> = xspider::METHODS.iter().map(|m| (*m).to_string()).collect();
    assert_eq!(reported, expected);
}
