//! # xspider-core
//!
//! X-Spider 的**共享内核**：HTTP 客户端、凭据注入、限流闸门与 429 熔断、请求签名
//! （`x-client-transaction-id`）、错误分类、时间归一化。
//!
//! ## 为什么这层必须单独存在
//!
//! 取数（`xspider-fetch`）与下载（`xspider-download`）的认证、配额、生命周期、
//! 测试面完全不同（见 `docs/01-ARCHITECTURE.md` §1），所以拆成两个组件；
//! 但它们**共享**的只有这几样底层能力。不抽出来，限流就会被写两遍，
//! 而 429 是这条链上最容易翻车的地方。
//!
//! 依赖方向是单向的：`组件 → 内核`。**内核不许依赖任何组件。**
//!
//! ## 三条容易违反的约束
//!
//! 1. 凭据只进不出：见 [`creds`]（`Debug` 被手工脱敏，访问器是 `pub(crate)`）。
//! 2. 一切请求过闸门：见 [`stack::HttpStack`]；绕过它等于绕过限流。
//! 3. 默认可离线测：真网络只是 [`transport::Transport`] 的一个实现，
//!    fixture 回放是另一个，见 [`fixture`]。

pub mod cancel;
pub mod creds;
pub mod error;
pub mod fixture;
pub mod http;
pub mod paging;
pub mod ratelimit;
pub mod stack;
pub mod transport;
pub mod url;
pub mod xclid;
pub mod xdate;

pub use cancel::CancelToken;
pub use creds::Credentials;
pub use error::{
    err_envelope, err_envelope_string, ok_envelope, ErrorCode, ErrorObject, XError, XResult,
};
pub use fixture::{CursorMatch, ReplayTransport};
pub use paging::{classify_cursor, CursorOutcome, Page, SeenIds};
pub use ratelimit::{GateStatus, Limits, RateGate, RequestClass};
pub use stack::HttpStack;
pub use transport::{HttpMethod, HttpRequest, HttpResponse, Transport};

/// 对外契约版本（`xspider_version()` 的返回值）。
///
/// 语义化版本；**method 与字段只增不改不删**（见 `docs/CONTRACT.md` §版本与兼容）。
pub const CONTRACT_VERSION: &str = "1.5.2";

/// 本仓库的构建版本（用于日志与排障，不参与握手）。
pub const BUILD_VERSION: &str = env!("CARGO_PKG_VERSION");
