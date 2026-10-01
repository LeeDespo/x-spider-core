//! # xspider-ffi
//!
//! 对外契约的实现层：C ABI（[`abi`]）+ method 派发（[`engine`]）。
//!
//! ## 两种交付形态共用这一份代码
//!
//! | 形态 | 产物 | 谁调用 |
//! |---|---|---|
//! | sidecar（主） | `xspiderd` 可执行文件 → 本地 JSON-RPC | 任何语言的外壳 |
//! | cdylib（次） | `libxspider.dylib` / `.so` / `.dll` → 三个 C 函数 | 需要进程内调用的外壳 |
//!
//! `bins/xspiderd` 依赖本 crate 的 **rlib** 并直接调用 [`Engine::call`]，
//! 所以"双形态行为一致"是**结构性保证**，不是人工对齐两份实现
//! （`docs/04-TESTING-AND-FIXTURES.md` §4）。
//!
//! ## 为什么主形态是 sidecar
//!
//! 有实测依据，不是偏好：一旦外壳启用 hardened runtime（公证的前提），
//! `dlopen` 任何 ad-hoc 签名的 dylib 都会被拒（`mapping process and mapped file
//! have different Team IDs`），而 sidecar 各自签名、互不验证。
//! 详见 `docs/03-FFI-SIGNING-PACKAGING.md` §1。
//!
//! ## 入口形状不可协商
//!
//! ```text
//! xspider_version() -> char*
//! xspider_call(method, json_in) -> json_out
//! xspider_free(char*)
//! ```
//!
//! 新增能力 = 新增一个 method 字符串（加法式演进，外壳无需改动）。
//! **不要**在这里导出 Rust 泛型 / 结构体布局相关的接口（`docs/03` §2）。

mod abi;
mod engine;

pub use abi::{call_as_json, envelope_parts, xspider_call, xspider_free, xspider_version};
pub use engine::{engine_from_env, Engine, Transport, METHODS};
