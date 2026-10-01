//! 守卫：**这个 CLI 不许依赖任何 `xspider-*` crate**。
//!
//! `xspider-cli` 存在的全部意义是当"外壳的替身"——它只能通过契约
//! （本地 HTTP JSON-RPC）使用组件，就像别的语言写的 shell 一样。
//! 一旦它 `use xspider_fetch::Post`，下面这些都会静默失效：
//!
//! - 契约缺字段 → "反正 Rust 里拿得到"，没人发现；
//! - 形状别扭（比如 `job_id` 得自己拼、时间没有时区信息）→ 同理；
//! - 双形态一致性 → 它会变成"只对 sidecar 有效"的验证。
//!
//! 这条测试**读它自己的 Cargo.toml**，所以是绕不过去的：想加依赖就得先删掉这条。

#[test]
fn the_cli_depends_on_the_contract_alone() {
    let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("读不到自己的 Cargo.toml");
    let mut section = String::new();
    let mut offenders: Vec<String> = Vec::new();

    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            section = trimmed.to_string();
            continue;
        }
        let is_dep_section = section.starts_with("[dependencies]")
            || section.starts_with("[dev-dependencies]")
            || section.starts_with("[build-dependencies]");
        if is_dep_section && trimmed.starts_with("xspider") {
            offenders.push(format!("{section} {trimmed}"));
        }
    }

    assert!(
        offenders.is_empty(),
        "xspider-cli 只能依赖客户端库（tokio / serde_json / reqwest），\
         不许依赖组件本身——它是外壳的替身，必须只看得见契约。违规行：{offenders:?}"
    );
}

/// 契约的三种形态里，这个 CLI 只用主形态（HTTP）；三条入口的载荷形状一致
/// 这件事由 `bins/xspiderd/tests/contract_dual.rs` 保证。
/// 这条测试只是把"我们知道自己在验证哪一种"写下来，避免以后误以为是全覆盖。
#[test]
fn the_cli_exercises_the_primary_transport() {
    let source = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/sidecar.rs"))
        .expect("读不到 sidecar.rs");
    assert!(
        source.contains("X-XSpider-Token"),
        "主形态是本地 HTTP JSON-RPC（docs/CONTRACT.md §2）：得带 token 头"
    );
    assert!(
        source.contains("ready "),
        "握手靠 stdout 的 ready 行（docs/CONTRACT.md §2.2）"
    );
}
