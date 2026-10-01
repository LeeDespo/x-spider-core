//! `x-client-transaction-id` 生成（对应 `docs/02-X-DOMAIN-NOTES.md` §A4）。
//!
//! X 自 2025 年起对 `/i/api/` 强制校验这个头；缺它 → 401。算法来自
//! `twscrape/xclid.py`（源自 `iSarabjitDhiman/XClientTransaction`，MIT），
//! 本文件是 `x-spider-mac` 里**已实跑通过**的 Swift 版
//! （`XClientTransaction.swift`）的 Rust 移植。
//!
//! ## 移植纪律
//!
//! 这是「**逐字对齐**」意义上的移植，不是重写：包括几处看起来奇怪的细节
//! （`float_to_hex` 输出大写、只有 `.` 分支被小写化；`"" → "0"` 的兜底）
//! 都**原样保留**，因为它们影响参与 SHA256 的字符串，进而影响 X 是否接受这个头。
//! 改动前请先读 `docs/02-X-DOMAIN-NOTES.md` §0「移植纪律」。
//!
//! ## 单独成模块、单独测
//!
//! 变易面（首页 HTML、bundle 结构）与稳定面（数学部分）被拆开了：
//! - `PageArtifacts` 负责**不稳定**的解析；
//! - `transaction_id` / `anim_key` / `calc_anim_key_string` / `Cubic` 是**纯函数**，
//!   可以在没有网络的测试里钉住数值行为。

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::{STANDARD, URL_SAFE};
use base64::Engine as _;
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{XError, XResult};
use crate::transport::HttpMethod;

/// 移植自上游的常量：与请求头一起构成 X 眼中的"客户端身份"。
pub const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36";
/// `twscrape/account.py` 的 TOKEN（X 轮换后的有效 Bearer；上游 2024 硬编码版已 401）。
pub const BEARER: &str = "Bearer AAAAAAAAAAAAAAAAAAAAANRILgAAAAAAnNwIzUejRCOuH5E6I8xnZz4puTs%3D1Zv7ttfk8LF81IUq16cHjhLTvJu4FA33AGWWjCpTnA";

/// 时间戳基准（`docs/02` 的上游实现用的魔术常量，不可改）。
const TS_EPOCH_OFFSET: i64 = 1_682_924_400;
/// 密钥缓存时长。上游注释说 vk 有效期较长，缓存 1 小时。
const KEY_TTL: Duration = Duration::from_secs(3600);
/// 扫描 bundle 找签名脚本时的上限（防御性：避免页面结构变化导致抓几百个 JS）。
const MAX_SCRIPT_PROBES: usize = 40;
/// 探测页。任何登录态用户页都行；关键是**登录态**渲染（未登录会命中登出版页面）。
pub const DEFAULT_PROBE_PATH: &str = "/tesla";

fn meta_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"<meta[^>]*name="twitter-site-verification"[^>]*content="([^"]*)""#)
            .expect("内置正则必须编译通过")
    })
}

fn svg_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?s)<svg[^>]*id="loading-x-anim[^"]*"[^>]*>(.*?)</svg>"#)
            .expect("内置正则必须编译通过")
    })
}

fn path_d_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"<path[^>]*d="([^"]*)""#).expect("内置正则必须编译通过"))
}

fn number_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"-?\d+(?:\.\d+)?").expect("内置正则必须编译通过"))
}

fn script_direct_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"https://[\w.-]+/x-web/[\w./-]+\.js"#).expect("内置正则必须编译通过")
    })
}

fn script_legacy_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"https://[\w.-]+/responsive-web/client-web/[\w./-]+\.js"#)
            .expect("内置正则必须编译通过")
    })
}

fn main_js_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"/client-web/main\.([^."']+)\.js"#).expect("内置正则必须编译通过")
    })
}

fn hash_map_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(\d+):"([0-9a-f]{7}|[0-9a-f]{16})""#).expect("内置正则必须编译通过")
    })
}

fn name_map_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(\d+):"([^"]+)""#).expect("内置正则必须编译通过"))
}

fn hex_full_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[0-9a-f]{7}$|^[0-9a-f]{16}$").expect("内置正则必须编译通过"))
}

fn indices_file_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?:\.{0,2}/)?[\w./-]*?\b(?:ondemand\.s|sign\.o)[\w.-]*\.js")
            .expect("内置正则必须编译通过")
    })
}

fn indices_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(\(\w{1}\[(\d{1,2})\],\s*16\))+").expect("内置正则必须编译通过"))
}

// ---------------------------------------------------------------------------
// 首页解析（不稳定面）
// ---------------------------------------------------------------------------

/// 从登录态首页里抠出来的所有签名原料。
///
/// 这个结构可以直接序列化落盘：`fixtures/xclid/page_artifacts.json` 存的就是
/// **真实页面**抠出来的这几个值（不是手写的），从而让 `anim_key` 的计算
/// 可以在离线测试里被钉住。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageArtifacts {
    /// `<meta name="twitter-site-verification">` 的 base64 内容。
    pub vk_b64: String,
    /// 上面 base64 解码后的字节。落在 fixture 里便于比对。
    pub vk: Vec<u8>,
    /// 各 `loading-x-anim-*` SVG 的第 2 条 `<path d>`。
    pub svg_paths: Vec<String>,
    /// 页面里出现（或由 webpack map 重建出）的脚本 URL。
    pub script_urls: Vec<String>,
}

impl PageArtifacts {
    pub fn parse(html: &str) -> XResult<Self> {
        let vk_b64 = meta_re()
            .captures(html)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().to_string())
            .ok_or_else(|| {
                XError::parse(
                    "xclid.verification_key",
                    "登录态页面里没有 twitter-site-verification",
                )
            })?;

        let vk = STANDARD
            .decode(vk_b64.trim())
            .or_else(|_| URL_SAFE.decode(vk_b64.trim()))
            .map_err(|e| {
                XError::parse(
                    "xclid.verification_key",
                    format!("twitter-site-verification 不是合法 base64：{e}"),
                )
            })?;
        if vk.len() < 6 {
            return Err(XError::parse(
                "xclid.verification_key",
                format!("verification key 太短（{} 字节）", vk.len()),
            ));
        }

        let svg_paths = parse_svg_paths(html);
        if svg_paths.is_empty() {
            return Err(XError::parse(
                "xclid.animation",
                "页面里没有 loading-x-anim 的动画帧数据",
            ));
        }

        Ok(Self {
            vk_b64,
            vk,
            svg_paths,
            script_urls: script_urls(html),
        })
    }

    /// 动画帧矩阵：`vk[5] % 路径数` 选中一条 `d`，去掉前 9 字符后按 `C` 切分成行。
    pub fn anim_rows(&self) -> XResult<Vec<Vec<f64>>> {
        let idx = usize::from(self.vk[5]) % self.svg_paths.len();
        let d = &self.svg_paths[idx];
        if d.len() <= 9 {
            return Err(XError::parse(
                "xclid.animation",
                format!("动画路径太短（{} 字符）", d.len()),
            ));
        }
        let clean = &d[9..];
        let mut rows = Vec::new();
        for seg in clean.split('C') {
            let row: Vec<f64> = number_re()
                .find_iter(seg)
                .filter_map(|m| m.as_str().parse::<f64>().ok())
                .collect();
            rows.push(row);
        }
        Ok(rows)
    }

    /// 页面里就直接给出的签名脚本 URL（命中 `ondemand.s*` / `sign.o-*` 的那一个）。
    pub fn direct_indices_url(&self) -> Option<String> {
        self.script_urls
            .iter()
            .find(|u| indices_file_re().is_match(u))
            .cloned()
    }

    /// 过滤出 x-web 的脚本；若页面是登出版，直接判为凭据问题。
    pub fn signing_scripts(&self) -> XResult<Vec<String>> {
        let xweb: Vec<String> = self
            .script_urls
            .iter()
            .filter(|u| u.contains("/x-web/"))
            .cloned()
            .collect();
        if xweb.is_empty() {
            return Ok(self.script_urls.clone());
        }
        if xweb.iter().any(|u| u.contains("entry-client-logged-out")) {
            return Err(XError::unauthorized(
                "cookie 无效或已登出：X 返回的是登出版页面",
            ));
        }
        Ok(xweb)
    }
}

/// SVG 动画帧：每个 `loading-x-anim-*` 的第 2 条 path 的 `d`。
fn parse_svg_paths(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    for caps in svg_re().captures_iter(html) {
        let Some(inner) = caps.get(1) else { continue };
        let ds: Vec<&str> = path_d_re()
            .captures_iter(inner.as_str())
            .filter_map(|c| c.get(1))
            .map(|m| m.as_str())
            .collect();
        if ds.len() >= 2 {
            out.push(ds[1].to_string());
        }
    }
    out
}

/// 页面的脚本清单：直链 + `main.<hash>a.js` + webpack map 重建出的 chunk。
fn script_urls(html: &str) -> Vec<String> {
    let mut urls: Vec<String> = Vec::new();
    for re in [script_direct_re(), script_legacy_re()] {
        urls.extend(re.find_iter(html).map(|m| m.as_str().to_string()));
    }
    if let Some(caps) = main_js_re().captures(html) {
        if let Some(m) = caps.get(1) {
            urls.push(format!(
                "https://abs.twimg.com/responsive-web/client-web/main.{}a.js",
                m.as_str()
            ));
        }
    }

    let mut hash_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for caps in hash_map_re().captures_iter(html) {
        if let (Some(k), Some(v)) = (caps.get(1), caps.get(2)) {
            hash_map.insert(k.as_str().to_string(), v.as_str().to_string());
        }
    }
    for caps in name_map_re().captures_iter(html) {
        let (Some(k), Some(v)) = (caps.get(1), caps.get(2)) else {
            continue;
        };
        let value = v.as_str();
        if hex_full_re().is_match(value) {
            continue;
        }
        let chunk_id = k.as_str();
        let hash = hash_map
            .get(chunk_id)
            .map(String::as_str)
            .unwrap_or(chunk_id);
        urls.push(format!(
            "https://abs.twimg.com/responsive-web/client-web/{value}.{hash}a.js"
        ));
    }

    let mut seen = std::collections::HashSet::new();
    urls.retain(|u| seen.insert(u.clone()));
    urls
}

/// Python `urljoin` 的核心语义。
pub fn urljoin(base: &str, rel: &str) -> String {
    if rel.starts_with("http://") || rel.starts_with("https://") {
        return rel.to_string();
    }
    if let Some(rest) = rel.strip_prefix("//") {
        return format!("https://{rest}");
    }
    let (scheme, after_scheme) = match base.split_once("://") {
        Some(parts) => parts,
        None => return rel.to_string(),
    };
    let host_end = after_scheme.find('/').unwrap_or(after_scheme.len());
    let host = &after_scheme[..host_end];
    let base_path = &after_scheme[host_end..];
    if rel.starts_with('/') {
        return format!("{scheme}://{host}{rel}");
    }
    let dir_end = base_path.rfind('/').map(|i| i + 1).unwrap_or(0);
    format!("{scheme}://{host}{}{rel}", &base_path[..dir_end])
}

/// 从签名脚本里抠出动画索引数组。
pub fn extract_indices(script_text: &str) -> Vec<usize> {
    indices_re()
        .captures_iter(script_text)
        .filter_map(|c| c.get(2))
        .filter_map(|m| m.as_str().parse::<usize>().ok())
        .collect()
}

// ---------------------------------------------------------------------------
// animKey 计算（稳定面）
// ---------------------------------------------------------------------------

/// `animKey = calc_anim_key(indices, rows, vk)`。
pub fn anim_key(indices: &[usize], rows: &[Vec<f64>], vk: &[u8]) -> XResult<String> {
    let first = *indices
        .first()
        .ok_or_else(|| XError::parse("xclid.indices", "签名脚本里没有动画索引"))?;
    if first >= vk.len() {
        return Err(XError::parse(
            "xclid.indices",
            format!("索引 {first} 越出 verification key（{} 字节）", vk.len()),
        ));
    }

    let mut frame_time = 1.0f64;
    for x in indices.iter().skip(1) {
        if *x >= vk.len() {
            return Err(XError::parse(
                "xclid.indices",
                format!("索引 {x} 越出 verification key（{} 字节）", vk.len()),
            ));
        }
        frame_time *= f64::from(vk[*x] % 16);
    }
    frame_time = (frame_time / 10.0 + 0.5).floor() * 10.0;

    let frame_idx = usize::from(vk[first] % 16);
    let row = rows
        .get(frame_idx)
        .or_else(|| rows.first())
        .ok_or_else(|| XError::parse("xclid.animation", "动画帧矩阵为空"))?;
    if row.is_empty() {
        return Err(XError::parse("xclid.animation", "动画帧行为空"));
    }
    Ok(calc_anim_key_string(row, frame_time / 4096.0))
}

/// 由一行动画帧数据 + 目标时间点算出 animKey 字符串。
///
/// 这里的每一处**看起来可以"优化"的地方都不能动**（见模块头注释）：
/// 大写十六进制、只小写 `.` 分支、`"" → "0"`，都会改变参与 SHA256 的字符串。
pub fn calc_anim_key_string(row: &[f64], target_time: f64) -> String {
    if row.len() < 7 {
        return String::new();
    }
    let from_color = [row[0], row[1], row[2], 1.0];
    let to_color = [row[3], row[4], row[5], 1.0];
    let from_rotation = [0.0f64];
    let to_rotation = [solve(row[6], 60.0, 360.0, true)];

    let frames = &row[7..];
    let curves: Vec<f64> = frames
        .iter()
        .enumerate()
        .map(|(i, x)| solve(*x, if i % 2 == 0 { 0.0 } else { -1.0 }, 1.0, false))
        .collect();
    if curves.len() < 4 {
        return String::new();
    }
    let val = Cubic::new(&curves).get_value(target_time);

    let color: Vec<f64> = interpolate(&from_color, &to_color, val)
        .into_iter()
        .map(|c| c.clamp(0.0, 255.0))
        // Python round() 是银行家舍入
        .map(f64::round_ties_even)
        .collect();
    let rotation = interpolate(&from_rotation, &to_rotation, val);
    let matrix = rotation_matrix(rotation[0]);

    let mut parts: Vec<String> = color[..color.len().saturating_sub(1)]
        .iter()
        .map(|c| format!("{:x}", *c as i64))
        .collect();
    for value in matrix {
        let rounded = (value * 100.0).round() / 100.0;
        let abs_rounded = rounded.abs();
        let hex = float_to_hex(abs_rounded);
        if hex.starts_with('.') {
            parts.push(format!("0{}", hex.to_lowercase()));
        } else if hex.is_empty() {
            parts.push("0".to_string());
        } else {
            parts.push(hex);
        }
    }
    parts.push("0".to_string());
    parts.push("0".to_string());
    parts.join("").replace(['.', '-'], "")
}

/// 上游的 `solve`。`rounding=true` 走 floor，否则四舍六入五成双 (2 位小数)。
fn solve(value: f64, min_val: f64, max_val: f64, rounding: bool) -> f64 {
    let result = value * (max_val - min_val) / 255.0 + min_val;
    if rounding {
        result.floor()
    } else {
        (result * 100.0).round_ties_even() / 100.0
    }
}

fn interpolate(from: &[f64], to: &[f64], f: f64) -> Vec<f64> {
    from.iter()
        .zip(to.iter())
        .map(|(a, b)| a * (1.0 - f) + b * f)
        .collect()
}

fn rotation_matrix(rotation: f64) -> [f64; 4] {
    let rad = rotation * std::f64::consts::PI / 180.0;
    [rad.cos(), -rad.sin(), rad.sin(), rad.cos()]
}

/// 上游的 `float_to_hex`（**输出大写**，这是刻意的，别"修"）。
fn float_to_hex(x: f64) -> String {
    let mut result: Vec<char> = Vec::new();
    let mut quotient = x as i64;
    let fraction = x - quotient as f64;
    let mut value = x;

    while quotient > 0 {
        quotient = (value / 16.0) as i64;
        let remainder = (value - quotient as f64 * 16.0) as i64;
        result.insert(0, hex_digit(remainder));
        value = quotient as f64;
    }

    if fraction == 0.0 {
        return result.into_iter().collect();
    }

    result.push('.');
    let mut frac = fraction;
    let mut guard = 0;
    while frac > 0.0 && guard < 1000 {
        frac *= 16.0;
        let integer = frac as i64;
        frac -= integer as f64;
        result.push(hex_digit(integer));
        guard += 1;
    }
    result.into_iter().collect()
}

fn hex_digit(value: i64) -> char {
    if value > 9 {
        char::from_u32((55 + value) as u32).unwrap_or('0')
    } else {
        char::from_digit(value as u32, 10).unwrap_or('0')
    }
}

/// 三次贝塞尔求值（上游 `Cubic`）。
#[derive(Debug, Clone)]
pub struct Cubic<'a> {
    curves: &'a [f64],
}

impl<'a> Cubic<'a> {
    pub fn new(curves: &'a [f64]) -> Self {
        Self { curves }
    }

    pub fn get_value(&self, time: f64) -> f64 {
        let c = self.curves;
        let mut start_gradient: f64 = 0.0;
        let mut end_gradient: f64 = 0.0;
        let mut start: f64 = 0.0;
        let mut mid: f64 = 0.0;
        let mut end: f64 = 1.0;

        if time <= 0.0 {
            if c[0] > 0.0 {
                start_gradient = c[1] / c[0];
            } else if c[1] == 0.0 && c.len() > 2 && c[2] > 0.0 {
                start_gradient = c[3] / c[2];
            }
            return start_gradient * time;
        }
        if time >= 1.0 {
            if c.len() > 2 && c[2] < 1.0 {
                end_gradient = (c[3] - 1.0) / (c[2] - 1.0);
            } else if c.len() > 2 && c[2] == 1.0 && c[0] < 1.0 {
                end_gradient = (c[1] - 1.0) / (c[0] - 1.0);
            }
            return 1.0 + end_gradient * (time - 1.0);
        }

        // 二分。加迭代上限：上游用浮点相等收敛，理论上不会死循环，
        // 但一旦真出现就会挂住整个 sidecar —— 挂死比微小数值偏差更糟。
        let mut guard = 0;
        while start < end && guard < 1000 {
            guard += 1;
            mid = (start + end) / 2.0;
            let x_est = cubic_calculate(c[0], c[2], mid);
            if (time - x_est).abs() < 0.00001 {
                return cubic_calculate(c[1], c[3], mid);
            }
            if x_est < time {
                start = mid;
            } else {
                end = mid;
            }
        }
        cubic_calculate(c[1], c[3], mid)
    }
}

fn cubic_calculate(a: f64, b: f64, m: f64) -> f64 {
    3.0 * a * (1.0 - m) * (1.0 - m) * m + 3.0 * b * (1.0 - m) * m * m + m * m * m
}

// ---------------------------------------------------------------------------
// 每请求的 transaction id
// ---------------------------------------------------------------------------

/// 由密钥 + 方法 + 路径 + 时间戳算出 `x-client-transaction-id`。
///
/// `now_unix_s` 显式传入，便于测试（生产代码传当前时间）。
pub fn transaction_id(
    method: HttpMethod,
    path: &str,
    vk: &[u8],
    anim_key: &str,
    now_unix_s: i64,
) -> String {
    let ts = now_unix_s - TS_EPOCH_OFFSET;
    let ts_bytes = [
        (ts & 0xFF) as u8,
        ((ts >> 8) & 0xFF) as u8,
        ((ts >> 16) & 0xFF) as u8,
        ((ts >> 24) & 0xFF) as u8,
    ];
    let payload = format!(
        "{}!{}!{}obfiowerehiring{}",
        method.as_str().to_uppercase(),
        path,
        ts,
        anim_key
    );
    let digest = Sha256::digest(payload.as_bytes());

    let mut bytes: Vec<u8> = Vec::with_capacity(vk.len() + 4 + 16 + 1 + 1);
    let noise: u8 = rand::random();
    bytes.push(noise);
    bytes.extend(vk.iter().map(|b| b ^ noise));
    bytes.extend(ts_bytes.iter().map(|b| b ^ noise));
    bytes.extend(digest[..16].iter().map(|b| b ^ noise));
    bytes.push(3 ^ noise);

    STANDARD.encode(&bytes).trim_end_matches('=').to_string()
}

// ---------------------------------------------------------------------------
// 状态：密钥缓存 + 加载
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Keys {
    vk: Vec<u8>,
    anim_key: String,
    loaded_at: Instant,
}

/// 签名密钥的持有者。密钥过期（1 小时）或自愈触发时重新加载。
#[derive(Debug, Default)]
pub struct Signer {
    keys: Mutex<Option<Keys>>,
}

impl Signer {
    pub fn new() -> Self {
        Self::default()
    }

    /// 丢弃缓存，下次请求会重新加载（401 自愈时用）。
    pub fn invalidate(&self) {
        if let Ok(mut guard) = self.keys.lock() {
            *guard = None;
        }
    }

    /// 生成一个 transaction id；密钥未就绪或已过期时返回 `None`。
    pub fn transaction_id(&self, method: HttpMethod, path: &str) -> Option<String> {
        let guard = self.keys.lock().ok()?;
        let keys = guard.as_ref()?;
        if keys.loaded_at.elapsed() >= KEY_TTL {
            return None;
        }
        let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
        Some(transaction_id(method, path, &keys.vk, &keys.anim_key, now))
    }

    pub fn is_loaded(&self) -> bool {
        self.keys
            .lock()
            .map(|g| g.as_ref().is_some_and(|k| k.loaded_at.elapsed() < KEY_TTL))
            .unwrap_or(false)
    }

    /// 从登录态首页 + 签名脚本加载密钥。**这是唯一需要网络的入口。**
    pub async fn ensure_loaded(&self, fetch: Arc<FetchFn>, probe_path: &str) -> XResult<()> {
        if self.is_loaded() {
            return Ok(());
        }

        // 1. 登录态页面
        let page = fetch(HttpRequestSpec {
            method: HttpMethod::Get,
            url: format!("https://x.com{probe_path}"),
            with_credentials: true,
        })
        .await?;
        if !(200..300).contains(&page.status) {
            return Err(XError::parse(
                "xclid.page",
                format!("探测页返回 HTTP {}", page.status),
            ));
        }
        if page.body_text().trim().is_empty() {
            return Err(XError::parse("xclid.page", "探测页返回了空内容"));
        }
        let artifacts = PageArtifacts::parse(&page.body_text())?;

        // 2. 找到签名脚本 → 动画索引
        let text_fetch: TextFetchFn = {
            let fetch = fetch.clone();
            Arc::new(move |url: String| {
                let fetch = fetch.clone();
                Box::pin(async move {
                    let resp = fetch(HttpRequestSpec {
                        method: HttpMethod::Get,
                        url,
                        with_credentials: false,
                    })
                    .await?;
                    Ok(resp.body_text())
                })
            })
        };
        let indices = artifacts.find_script_indices(text_fetch).await?;

        // 3. animKey
        let rows = artifacts.anim_rows()?;
        let key = anim_key(&indices, &rows, &artifacts.vk)?;
        if key.is_empty() {
            return Err(XError::parse("xclid.animation", "算出的 animKey 为空"));
        }

        if let Ok(mut guard) = self.keys.lock() {
            *guard = Some(Keys {
                vk: artifacts.vk,
                anim_key: key,
                loaded_at: Instant::now(),
            });
        }
        tracing::debug!("xclid 密钥已加载");
        Ok(())
    }
}

/// 签名加载期间取文本（脚本内容）。
pub type TextFetchFn = Arc<
    dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = XResult<String>> + Send>>
        + Send
        + Sync,
>;

/// 签名加载期间取响应（带指定凭据策略）。
pub type FetchFn = dyn Fn(
        HttpRequestSpec,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = XResult<crate::transport::HttpResponse>> + Send>,
    > + Send
    + Sync;

/// 整个「找签名脚本」阶段的总预算。
///
/// 上限是必须的：这段是**顺序**扫描最多 40 个 JS，每个都可能等到 30s 超时，
/// 没有总预算就等于给 sidecar 留了一条可以挂 20 分钟的路（`docs/05` §6）。
const FIND_INDICES_BUDGET: Duration = Duration::from_secs(45);

impl PageArtifacts {
    /// 找出签名脚本里的动画索引数组。
    ///
    /// 上游是 16 路并发 + 首个命中即止；这里刻意改成**顺序 + 总预算**：
    /// 这是 1 小时一次的操作，省下 spawn/JoinSet 的生命周期复杂度，
    /// 换来的是"行为更好推理"和一个可证明的上界。真的慢了再优化。
    pub async fn find_script_indices(&self, fetch: TextFetchFn) -> XResult<Vec<usize>> {
        let started = Instant::now();
        let scripts = self.signing_scripts()?;

        if let Some(url) = self.direct_indices_url() {
            let text = fetch(url).await?;
            let indices = extract_indices(&text);
            if !indices.is_empty() {
                return Ok(indices);
            }
        }

        for url in scripts.iter().take(MAX_SCRIPT_PROBES) {
            if started.elapsed() > FIND_INDICES_BUDGET {
                return Err(XError::parse(
                    "xclid.indices",
                    format!(
                        "在 {:?} 预算内没找到签名脚本（已试 {} 个）",
                        FIND_INDICES_BUDGET,
                        scripts.len().min(MAX_SCRIPT_PROBES)
                    ),
                ));
            }
            let text = match fetch(url.clone()).await {
                Ok(t) => t,
                // 单个 chunk 抓不到不该让整个加载失败
                Err(e) => {
                    tracing::debug!(error = %e, "扫描签名脚本失败，跳过");
                    continue;
                }
            };
            let Some(found) = indices_file_re().find(&text) else {
                continue;
            };
            let joined = urljoin(url, found.as_str());
            let script = fetch(joined).await?;
            let indices = extract_indices(&script);
            if !indices.is_empty() {
                return Ok(indices);
            }
        }

        Err(XError::parse(
            "xclid.indices",
            format!("扫描了 {} 个脚本都没找到动画索引", scripts.len()),
        ))
    }
}

/// `Signer` 取密钥时需要发出的请求（由调用方补上 cookie / UA 头）。
#[derive(Debug, Clone)]
pub struct HttpRequestSpec {
    pub method: HttpMethod,
    pub url: String,
    pub with_credentials: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 录制下来的"真实页面原料"。
    /// 用 fixture 而不是手写值：手写的只能证明「我按自己以为的格式算对了」。
    ///
    /// `script_indices` 在生产路径上来自网络抓到的签名脚本，
    /// 录制时一并落盘，才能让 animKey 的计算在离线测试里被钉住。
    #[derive(Deserialize)]
    struct PageFixture {
        #[serde(flatten)]
        artifacts: PageArtifacts,
        script_indices: Vec<usize>,
        captured_at: String,
    }

    fn real_page() -> PageFixture {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/xclid/page_artifacts.json"
        );
        let text = std::fs::read_to_string(path).expect(
            "缺少 fixtures/xclid/page_artifacts.json：用 \
             `XSPIDER_LIVE=1 cargo test -- --ignored record_xclid_page` 生成",
        );
        serde_json::from_str(&text).expect("page_artifacts.json 不合法")
    }

    #[test]
    fn real_page_artifacts_produce_a_stable_anim_key() {
        let fx = real_page();
        assert!(
            fx.captured_at.starts_with("20"),
            "fixture 必须带采集日期，改版样本靠它排时间线"
        );
        let rows = fx.artifacts.anim_rows().expect("动画帧必须能解析出来");
        assert!(!rows.is_empty());

        let key =
            anim_key(&fx.script_indices, &rows, &fx.artifacts.vk).expect("animKey 必须能算出来");
        assert!(!key.is_empty());
        // animKey 会被拼进参与 SHA256 的字符串，形态必须稳定
        assert!(
            key.chars().all(|c| c.is_ascii_hexdigit()),
            "animKey 只应由十六进制字符组成：{key}"
        );
    }

    #[test]
    fn anim_key_is_deterministic() {
        let fx = real_page();
        let rows = fx.artifacts.anim_rows().unwrap();
        assert_eq!(
            anim_key(&fx.script_indices, &rows, &fx.artifacts.vk).unwrap(),
            anim_key(&fx.script_indices, &rows, &fx.artifacts.vk).unwrap()
        );
    }

    #[test]
    fn anim_key_rejects_out_of_range_index() {
        let fx = real_page();
        let rows = fx.artifacts.anim_rows().unwrap();
        let err = anim_key(&[999usize], &rows, &fx.artifacts.vk).unwrap_err();
        assert_eq!(err.code(), crate::error::ErrorCode::Parse);
        assert!(err.to_string().contains("越出"), "{err}");
    }

    #[test]
    fn transaction_id_shape_is_stable() {
        // 固定 vk / animKey / 时间 → 输出只受随机首字节影响，长度与尾部固定
        let vk: Vec<u8> = (0..48u8).collect();
        let a = transaction_id(
            HttpMethod::Get,
            "/i/api/graphql/x/UserByScreenName",
            &vk,
            "deadbeef",
            1_800_000_000,
        );
        let b = transaction_id(
            HttpMethod::Get,
            "/i/api/graphql/x/UserByScreenName",
            &vk,
            "deadbeef",
            1_800_000_000,
        );
        assert!(!a.is_empty());
        assert!(!a.contains('='), "padding 必须被去掉");
        // 去掉随机首字节后，长度确定：1 + 48 + 4 + 16 + 1 = 70 字节 → ceil(70/3)*4 = 96，去 padding 94
        assert_eq!(a.len(), b.len(), "{a} vs {b}");
        assert_eq!(a.len(), 94);
        // 小写/大写都不该出现 '-' 或 '_'（标准 base64 字母表）
        assert!(!a.contains('-') && !a.contains('_'));
    }

    #[test]
    fn transaction_id_uses_the_documented_payload() {
        // 直接复算 payload，锁住"METHOD!path!ts+obfiowerehiring+animKey"这个形状
        let vk = vec![7u8; 48];
        let key = "abc123";
        let out = transaction_id(
            HttpMethod::Post,
            "/i/api/graphql/z/TweetDetail",
            &vk,
            key,
            TS_EPOCH_OFFSET + 1_000,
        );
        // 手工复算期望值
        let ts = 1_000i64;
        let ts_bytes = [
            (ts & 0xFF) as u8,
            ((ts >> 8) & 0xFF) as u8,
            ((ts >> 16) & 0xFF) as u8,
            ((ts >> 24) & 0xFF) as u8,
        ];
        let payload = format!("POST!/i/api/graphql/z/TweetDetail!1000obfiowerehiring{key}");
        let digest = Sha256::digest(payload.as_bytes());
        let mut bytes = vec![];
        let mut raw = vk.clone();
        raw.extend(ts_bytes);
        raw.extend(&digest[..16]);
        raw.push(3);
        // 随机首字节未知 → 只校验"去噪后"的内容：用同样的噪声还原
        let decoded = STANDARD
            .decode(format!("{out}{}", "=".repeat((4 - out.len() % 4) % 4)))
            .expect("输出必须是合法 base64");
        let noise = decoded[0];
        bytes.push(noise);
        bytes.extend(raw.iter().map(|b| b ^ noise));
        assert_eq!(decoded, bytes);
    }

    #[test]
    fn cubic_endpoints_are_exact() {
        let curves = [0.25, 0.1, 0.25, 1.0];
        let c = Cubic::new(&curves);
        assert!(c.get_value(0.0).abs() < 1e-12);
        assert!((c.get_value(1.0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn cubic_terminates_and_stays_in_range() {
        let curves = [0.42, 0.0, 0.58, 1.0];
        let c = Cubic::new(&curves);
        for i in 0..=100 {
            let v = c.get_value(i as f64 / 100.0);
            assert!(v.is_finite());
            assert!((-0.001..=1.001).contains(&v), "v={v} t={i}");
        }
    }

    #[test]
    fn solve_matches_upstream_rounding() {
        assert_eq!(solve(255.0, 60.0, 360.0, true), 360.0);
        assert_eq!(solve(0.0, 60.0, 360.0, true), 60.0);
        // 非取整分支保留 2 位小数
        assert_eq!(solve(127.0, 0.0, 1.0, false), 0.5);
    }

    #[test]
    fn float_to_hex_is_uppercase_like_upstream() {
        assert_eq!(float_to_hex(0.0), "");
        assert_eq!(float_to_hex(255.0), "FF");
        assert_eq!(float_to_hex(10.0), "A");
        assert!(float_to_hex(0.5).starts_with('.'));
    }

    #[test]
    fn urljoin_handles_all_three_relative_forms() {
        let base = "https://abs.twimg.com/responsive-web/client-web/main.abc1234a.js";
        assert_eq!(urljoin(base, "https://x.com/a.js"), "https://x.com/a.js");
        assert_eq!(
            urljoin(base, "//cdn.example.com/a.js"),
            "https://cdn.example.com/a.js"
        );
        assert_eq!(
            urljoin(base, "/ondemand.s.js"),
            "https://abs.twimg.com/ondemand.s.js"
        );
        assert_eq!(
            urljoin(base, "ondemand.s.js"),
            "https://abs.twimg.com/responsive-web/client-web/ondemand.s.js"
        );
    }

    #[test]
    fn extract_indices_reads_the_upstream_pattern() {
        let script = r#"var a=1;(x[3],16)+(x[12],16)+(x[5],16);foo;(y[9], 16)"#;
        assert_eq!(extract_indices(script), vec![3, 12, 5, 9]);
        assert!(extract_indices("nothing here").is_empty());
    }

    #[test]
    fn parse_rejects_page_without_verification_key() {
        let err = PageArtifacts::parse("<html>no meta here</html>").unwrap_err();
        assert_eq!(err.code(), crate::error::ErrorCode::Parse);
    }

    #[test]
    fn logged_out_page_maps_to_unauthorized() {
        let html = format!(
            r#"<meta name="twitter-site-verification" content="{}">
               <svg id="loading-x-anim-0"><path d="M 1"/><path d="M 10,30 C 254,52 75,150 35,70 h 225 s 1,2 C 3,4 5,6 7,8"/></svg>
               <script src="https://abs.twimg.com/x-web/entry-client-logged-out.abc1234a.js"></script>"#,
            STANDARD.encode([1u8, 2, 3, 4, 5, 6, 7, 8])
        );
        let a = PageArtifacts::parse(&html).unwrap();
        let err = a.signing_scripts().unwrap_err();
        assert_eq!(err.code(), crate::error::ErrorCode::Unauthorized);
    }

    #[test]
    fn signer_reports_not_loaded_before_load() {
        let s = Signer::new();
        assert!(!s.is_loaded());
        assert!(s.transaction_id(HttpMethod::Get, "/x").is_none());
        s.invalidate();
        assert!(!s.is_loaded());
    }

    // -----------------------------------------------------------------
    // live 录制：抓真实页面 → 落盘成 fixture
    // -----------------------------------------------------------------

    /// ```bash
    /// XSPIDER_LIVE=1 XSPIDER_COOKIE='...' XSPIDER_PROXY=http://127.0.0.1:12450 \
    ///   cargo test -p xspider-core --lib -- --ignored --nocapture record_xclid_page
    /// ```
    ///
    /// 只落盘**从页面里抠出来的原料**（vk / SVG 路径 / 脚本 URL / 签名索引），
    /// 不落盘整页 HTML：那是一个随改版天天变的 1MB 大块，
    /// 对解析覆盖率的额外价值为零，反而会让 reviewer 无法 diff。
    #[tokio::test]
    #[ignore = "live：需要 XSPIDER_LIVE=1 与 XSPIDER_COOKIE（会抓一次 x.com 页面）"]
    async fn record_xclid_page() {
        use crate::cancel::CancelToken;
        use crate::creds::Credentials;
        use crate::http::ProxyConfig;
        use crate::stack::HttpStack;

        if std::env::var("XSPIDER_LIVE").as_deref() != Ok("1") {
            eprintln!("跳过：需要 XSPIDER_LIVE=1");
            return;
        }
        let Ok(cookie) = std::env::var("XSPIDER_COOKIE") else {
            eprintln!("跳过：需要 XSPIDER_COOKIE");
            return;
        };
        let proxy = match std::env::var("XSPIDER_PROXY") {
            Ok(url) if !url.trim().is_empty() => ProxyConfig::Manual(url.trim().to_string()),
            _ => ProxyConfig::Env,
        };

        let stack = HttpStack::live(proxy).expect("构造 HTTP 栈失败");
        stack.set_credentials(Some(
            Credentials::from_cookie(cookie, None).expect("cookie 里必须有 ct0"),
        ));
        let cancel = CancelToken::new();

        let page = stack
            .fetch_for_signer(
                &HttpRequestSpec {
                    method: HttpMethod::Get,
                    url: format!("https://x.com{DEFAULT_PROBE_PATH}"),
                    with_credentials: true,
                },
                &cancel,
            )
            .await
            .expect("抓取探测页失败");
        assert_eq!(page.status, 200, "探测页期望 HTTP 200");
        let artifacts =
            PageArtifacts::parse(&page.body_text()).expect("页面解析失败（X 可能改版）");

        let text_fetch: TextFetchFn = {
            let stack = stack.clone();
            let cancel = cancel.clone();
            Arc::new(move |url: String| {
                let stack = stack.clone();
                let cancel = cancel.clone();
                Box::pin(async move {
                    let resp = stack
                        .fetch_for_signer(
                            &HttpRequestSpec {
                                method: HttpMethod::Get,
                                url,
                                with_credentials: false,
                            },
                            &cancel,
                        )
                        .await?;
                    Ok(resp.body_text())
                })
            })
        };
        let indices = artifacts
            .find_script_indices(text_fetch)
            .await
            .expect("找不到签名脚本的动画索引");

        // 录一条算不出 animKey 的 fixture 没有意义，所以先自己算一遍
        let rows = artifacts.anim_rows().expect("动画帧解析失败");
        let key = anim_key(&indices, &rows, &artifacts.vk).expect("animKey 计算失败");
        assert!(!key.is_empty());

        let mut value = serde_json::to_value(&artifacts).expect("PageArtifacts 应可序列化");
        let obj = value.as_object_mut().expect("顶层应是对象");
        obj.insert("script_indices".into(), serde_json::json!(indices));
        obj.insert(
            "captured_at".into(),
            serde_json::json!(crate::xdate::today_utc()),
        );
        obj.insert(
            "note".into(),
            serde_json::json!(
                "由 record_xclid_page 从真实登录态页面抠出；vk 是站点级常量，不含个人数据"
            ),
        );

        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/xclid/page_artifacts.json"
        );
        if let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(parent).expect("创建 fixtures/xclid 失败");
        }
        std::fs::write(
            path,
            serde_json::to_string_pretty(&value).expect("序列化失败"),
        )
        .unwrap_or_else(|e| panic!("写 {path} 失败：{e}"));

        eprintln!(
            "录制完成：{}（vk {} 字节、{} 个 SVG 路径、{} 个脚本 URL、{} 个索引；animKey {} 字符）",
            path,
            artifacts.vk.len(),
            artifacts.svg_paths.len(),
            artifacts.script_urls.len(),
            indices.len(),
            key.len()
        );
    }
}
