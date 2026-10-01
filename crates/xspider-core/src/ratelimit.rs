//! 限流闸门与 429 熔断（铁律 6：限流是一等公民）。
//!
//! 三条设计约束：
//! 1. **GraphQL API 与媒体 CDN 是两个域、两套配额，必须分开治理**（docs/02 §B6）——
//!    所以闸门按 `RequestClass` 分桶，一边被限流不会把另一边冻住；
//! 2. **429 触发后熔断 + 冷却，而不是继续重试**（docs/02 §B7）——冷却期内直接快速失败，
//!    不会出现「越限越试把限流拖长」；
//! 3. **网络类异常不设粘性状态**（docs/02 §B5 的实测教训：断网时无成功请求 →
//!    状态永不清除 → 代理恢复后应用仍卡很久）。冷却一律用 `Instant` 表示，
//!    到期自然失效；成功一次立即复位。没有「需要人工清除的布尔标志」这种形态。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::{XError, XResult};

/// 请求类别。配额从属于域，不从属于组件。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestClass {
    /// `x.com/i/api/*`：受账号配额约束，管翻页/爬虫。
    Api,
    /// `pbs.twimg.com` / `video.twimg.com`：管图片视频下载。
    Cdn,
}

/// 可调限流参数（对应契约方法 `net.set_limits`）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Limits {
    /// API 端点平均速率（每秒请求数）。
    pub api_rps: f64,
    /// API 端点的突发额度（令牌桶容量）。
    pub api_burst: u32,
    /// CDN 并发上限。
    pub cdn_concurrency: u32,
    /// 上游没给 `Retry-After` 时的默认冷却秒数。
    pub cooldown_s: u64,
}

impl Default for Limits {
    fn default() -> Self {
        // 上游靠浏览器渲染节奏天然限速（页间 400–500ms，docs/02 §B1）。
        // Rust 循环没有这个节奏，必须显式等价出来。
        Self {
            api_rps: 2.0,
            api_burst: 4,
            cdn_concurrency: 4,
            cooldown_s: 120,
        }
    }
}

impl Limits {
    /// 夹到安全区间：rps 不能为 0（会除零），cooldown 不能为 0（失去熔断意义）。
    pub fn sanitized(self) -> Self {
        Self {
            api_rps: if self.api_rps.is_finite() && self.api_rps > 0.0 {
                self.api_rps.clamp(0.05, 50.0)
            } else {
                Limits::default().api_rps
            },
            api_burst: self.api_burst.max(1),
            cdn_concurrency: self.cdn_concurrency.max(1),
            cooldown_s: self.cooldown_s.max(1),
        }
    }
}

/// 冷却上限：即便上游要求等更久，也不能无限挂起（避免"代理恢复后还卡着"）。
const MAX_COOLDOWN: Duration = Duration::from_secs(15 * 60);
/// 连续 429 的退避放大上限（2^n）。
const MAX_ESCALATION: u32 = 4;

/// `acquire` 的结果。等待动作由调用方在锁外做，以便把取消信号一起竞速。
#[derive(Debug)]
pub enum GateOutcome {
    /// 拿到令牌，可以发请求。
    Acquired,
    /// 桶空了，需要等这么久再试。
    Wait(Duration),
    /// 处于 429 冷却期，快速失败。
    Cooling { retry_after_s: u64 },
}

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last_refill: Instant,
    cooldown_until: Option<Instant>,
    /// 连续 429 次数，用于退避放大；成功一次清零。
    consecutive_429: u32,
}

impl Bucket {
    fn new(burst: u32) -> Self {
        Self {
            tokens: f64::from(burst),
            last_refill: Instant::now(),
            cooldown_until: None,
            consecutive_429: 0,
        }
    }
}

/// 闸门快照，供 `net.status` 使用。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateStatus {
    /// `ok` | `rate_limited`
    pub state: &'static str,
    /// 冷却结束的 **Unix 秒**（UTC）。仅 `state == "rate_limited"` 时存在。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limited_until: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_s: Option<u64>,
}

#[derive(Debug)]
pub struct RateGate {
    buckets: Mutex<HashMap<RequestClass, Bucket>>,
    limits: Mutex<Limits>,
}

impl Default for RateGate {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl RateGate {
    pub fn new(limits: Limits) -> Self {
        let limits = limits.sanitized();
        let mut buckets = HashMap::new();
        buckets.insert(RequestClass::Api, Bucket::new(limits.api_burst));
        buckets.insert(RequestClass::Cdn, Bucket::new(limits.cdn_concurrency));
        Self {
            buckets: Mutex::new(buckets),
            limits: Mutex::new(limits),
        }
    }

    pub fn limits(&self) -> Limits {
        *self.limits.lock().expect("limits mutex poisoned")
    }

    /// 更新参数。令牌桶容量会被立即调整（调大时补满，避免"改了参数却还在等"）。
    pub fn set_limits(&self, limits: Limits) {
        let limits = limits.sanitized();
        *self.limits.lock().expect("limits mutex poisoned") = limits;
        let mut buckets = self.buckets.lock().expect("buckets mutex poisoned");
        let api = buckets
            .entry(RequestClass::Api)
            .or_insert_with(|| Bucket::new(limits.api_burst));
        api.tokens = api.tokens.min(f64::from(limits.api_burst));
        let cdn = buckets
            .entry(RequestClass::Cdn)
            .or_insert_with(|| Bucket::new(limits.cdn_concurrency));
        cdn.tokens = cdn.tokens.min(f64::from(limits.cdn_concurrency));
    }

    fn rps_for(limits: &Limits, class: RequestClass) -> f64 {
        match class {
            RequestClass::Api => limits.api_rps,
            RequestClass::Cdn => f64::from(limits.cdn_concurrency) * 2.0,
        }
    }

    fn capacity_for(limits: &Limits, class: RequestClass) -> u32 {
        match class {
            RequestClass::Api => limits.api_burst,
            RequestClass::Cdn => limits.cdn_concurrency,
        }
    }

    /// 尝试取一个令牌。**不做 await**——等待由调用方在锁外完成。
    pub fn acquire(&self, class: RequestClass) -> GateOutcome {
        let limits = self.limits();
        let now = Instant::now();
        let mut buckets = self.buckets.lock().expect("buckets mutex poisoned");
        let bucket = buckets
            .entry(class)
            .or_insert_with(|| Bucket::new(Self::capacity_for(&limits, class)));

        if let Some(until) = bucket.cooldown_until {
            let remaining = until.saturating_duration_since(now);
            if remaining.is_zero() {
                // 到期即自然放行——不需要任何人来"清除"状态
                bucket.cooldown_until = None;
            } else {
                return GateOutcome::Cooling {
                    retry_after_s: remaining.as_secs().max(1),
                };
            }
        }

        let rps = Self::rps_for(&limits, class);
        let capacity = f64::from(Self::capacity_for(&limits, class));
        let elapsed = now
            .saturating_duration_since(bucket.last_refill)
            .as_secs_f64();
        if elapsed > 0.0 {
            bucket.tokens = (bucket.tokens + elapsed * rps).min(capacity);
            bucket.last_refill = now;
        }

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            GateOutcome::Acquired
        } else {
            let deficit = 1.0 - bucket.tokens;
            GateOutcome::Wait(Duration::from_secs_f64(deficit / rps))
        }
    }

    /// 收到 429：按上游 `Retry-After` 冷却；没有就按 `cooldown_s`。
    /// 连续 429 会退避放大（×2^n，封顶 15 分钟），成功一次即清零。
    /// 返回实际生效的冷却秒数。
    pub fn note_rate_limited(&self, class: RequestClass, retry_after_s: Option<u64>) -> u64 {
        let limits = self.limits();
        let now = Instant::now();
        let mut buckets = self.buckets.lock().expect("buckets mutex poisoned");
        let bucket = buckets
            .entry(class)
            .or_insert_with(|| Bucket::new(Self::capacity_for(&limits, class)));

        bucket.consecutive_429 = bucket.consecutive_429.saturating_add(1);
        let escalation = 1u64 << bucket.consecutive_429.min(MAX_ESCALATION).saturating_sub(1);
        let base = retry_after_s
            .filter(|s| *s > 0)
            .unwrap_or(limits.cooldown_s);
        let seconds = base.saturating_mul(escalation);

        let cooldown = Duration::from_secs(seconds).min(MAX_COOLDOWN);
        bucket.cooldown_until = Some(now + cooldown);
        bucket.tokens = 0.0;
        bucket.last_refill = now;
        cooldown.as_secs().max(1)
    }

    /// 请求成功：立即复位冷却与退避计数（正常态这条路是零副作用的整数比较）。
    pub fn note_success(&self, class: RequestClass) {
        let mut buckets = self.buckets.lock().expect("buckets mutex poisoned");
        if let Some(bucket) = buckets.get_mut(&class) {
            if bucket.cooldown_until.is_some() || bucket.consecutive_429 != 0 {
                bucket.cooldown_until = None;
                bucket.consecutive_429 = 0;
            }
        }
    }

    /// 当前冷却剩余秒数（0 表示未冷却）。
    pub fn cooldown_remaining_s(&self, class: RequestClass) -> Option<u64> {
        let mut buckets = self.buckets.lock().expect("buckets mutex poisoned");
        let bucket = buckets.get_mut(&class)?;
        let until = bucket.cooldown_until?;
        let remaining = until.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bucket.cooldown_until = None;
            None
        } else {
            Some(remaining.as_secs().max(1))
        }
    }

    /// 供 `net.status` 的聚合快照。
    pub fn status(&self) -> GateStatus {
        let api = self.cooldown_remaining_s(RequestClass::Api);
        let cdn = self.cooldown_remaining_s(RequestClass::Cdn);
        let worst = match (api, cdn) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        match worst {
            Some(secs) => GateStatus {
                state: "rate_limited",
                rate_limited_until: SystemTime::now()
                    .checked_add(Duration::from_secs(secs))
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs()),
                retry_after_s: Some(secs),
            },
            None => GateStatus {
                state: "ok",
                rate_limited_until: None,
                retry_after_s: None,
            },
        }
    }

    /// 冷却期快速失败用的错误。
    pub fn cooling_error(&self, class: RequestClass, endpoint: Option<&str>) -> XError {
        let retry_after_s = self.cooldown_remaining_s(class).unwrap_or(1);
        XError::RateLimited {
            retry_after_s,
            endpoint: endpoint.map(str::to_string),
        }
    }
}

impl RateGate {
    /// 便捷：按闸门规则等待或失败。`sleep` 期间可被取消。
    pub async fn acquire_or_wait(
        &self,
        class: RequestClass,
        endpoint: Option<&str>,
        cancel: &crate::cancel::CancelToken,
    ) -> XResult<()> {
        loop {
            match self.acquire(class) {
                GateOutcome::Acquired => return Ok(()),
                GateOutcome::Cooling { retry_after_s } => {
                    return Err(XError::RateLimited {
                        retry_after_s,
                        endpoint: endpoint.map(str::to_string),
                    });
                }
                GateOutcome::Wait(wait) => {
                    // 加一点余量，避免刚醒又差一点点
                    let wait = wait + Duration::from_millis(5);
                    match cancel.race(tokio::time::sleep(wait)).await {
                        Ok(()) => continue,
                        Err(e) => return Err(e),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burst_then_wait() {
        let gate = RateGate::new(Limits {
            api_rps: 1.0,
            api_burst: 2,
            cdn_concurrency: 2,
            cooldown_s: 10,
        });
        assert!(matches!(
            gate.acquire(RequestClass::Api),
            GateOutcome::Acquired
        ));
        assert!(matches!(
            gate.acquire(RequestClass::Api),
            GateOutcome::Acquired
        ));
        match gate.acquire(RequestClass::Api) {
            GateOutcome::Wait(d) => assert!(d <= Duration::from_secs(1), "{d:?}"),
            other => panic!("第三个请求应当等令牌，实际 {other:?}"),
        }
    }

    #[test]
    fn classes_are_independent() {
        let gate = RateGate::new(Limits::default());
        gate.note_rate_limited(RequestClass::Api, Some(60));
        assert!(gate.cooldown_remaining_s(RequestClass::Api).is_some());
        // CDN 那一侧不该被 API 的限流冻住（docs/02 §B6）
        assert_eq!(gate.cooldown_remaining_s(RequestClass::Cdn), None);
        assert!(matches!(
            gate.acquire(RequestClass::Cdn),
            GateOutcome::Acquired
        ));
    }

    #[test]
    fn rate_limit_fails_fast_and_honors_retry_after() {
        let gate = RateGate::new(Limits::default());
        let secs = gate.note_rate_limited(RequestClass::Api, Some(42));
        assert_eq!(secs, 42);
        match gate.acquire(RequestClass::Api) {
            GateOutcome::Cooling { retry_after_s } => {
                assert!((41..=42).contains(&retry_after_s), "{retry_after_s}");
            }
            other => panic!("冷却期内必须快速失败，实际 {other:?}"),
        }
    }

    #[test]
    fn cooldown_defaults_to_config_when_no_retry_after() {
        let gate = RateGate::new(Limits {
            cooldown_s: 7,
            ..Limits::default()
        });
        assert_eq!(gate.note_rate_limited(RequestClass::Api, None), 7);
    }

    #[test]
    fn consecutive_429_escalates_and_success_resets() {
        let gate = RateGate::new(Limits {
            cooldown_s: 10,
            ..Limits::default()
        });
        assert_eq!(gate.note_rate_limited(RequestClass::Api, None), 10);
        assert_eq!(gate.note_rate_limited(RequestClass::Api, None), 20);
        assert_eq!(gate.note_rate_limited(RequestClass::Api, None), 40);
        gate.note_success(RequestClass::Api);
        assert_eq!(gate.cooldown_remaining_s(RequestClass::Api), None);
        assert_eq!(gate.note_rate_limited(RequestClass::Api, None), 10);
    }

    #[test]
    fn cooldown_is_capped() {
        let gate = RateGate::new(Limits::default());
        for _ in 0..10 {
            gate.note_rate_limited(RequestClass::Api, Some(3600));
        }
        let remaining = gate.cooldown_remaining_s(RequestClass::Api).unwrap();
        assert!(remaining <= MAX_COOLDOWN.as_secs(), "{remaining}");
    }

    #[test]
    fn status_reports_ok_when_never_limited() {
        let gate = RateGate::new(Limits::default());
        let s = gate.status();
        assert_eq!(s.state, "ok");
        assert!(s.rate_limited_until.is_none());
        assert!(s.retry_after_s.is_none());
    }

    #[test]
    fn status_reports_until_timestamp() {
        let gate = RateGate::new(Limits::default());
        gate.note_rate_limited(RequestClass::Api, Some(30));
        let s = gate.status();
        assert_eq!(s.state, "rate_limited");
        let until = s.rate_limited_until.unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(until > now && until <= now + 31, "until={until} now={now}");
    }

    /// docs/02 §B5 的核心回归：**网络异常不得留下粘性限流状态**。
    /// 曾经的表现是「代理恢复了，应用还卡很久，除非重启」。
    #[test]
    fn transport_failures_leave_no_sticky_state() {
        let gate = RateGate::new(Limits::default());
        // 模拟一串网络异常：闸门只被"取令牌"，从不被 note_rate_limited
        for _ in 0..100 {
            let _ = gate.acquire(RequestClass::Api);
        }
        assert_eq!(gate.status().state, "ok");
        assert_eq!(gate.cooldown_remaining_s(RequestClass::Api), None);
    }

    #[test]
    fn sanitize_rejects_zero_rps() {
        let l = Limits {
            api_rps: 0.0,
            api_burst: 0,
            cdn_concurrency: 0,
            cooldown_s: 0,
        }
        .sanitized();
        assert!(l.api_rps > 0.0);
        assert!(l.api_burst >= 1);
        assert!(l.cdn_concurrency >= 1);
        assert!(l.cooldown_s >= 1);
    }

    #[tokio::test]
    async fn acquire_or_wait_is_cancellable() {
        let gate = RateGate::new(Limits {
            api_rps: 0.2, // 5 秒一个令牌
            api_burst: 1,
            cdn_concurrency: 1,
            cooldown_s: 10,
        });
        assert!(gate
            .acquire_or_wait(RequestClass::Api, None, &crate::cancel::CancelToken::new())
            .await
            .is_ok());
        let token = crate::cancel::CancelToken::new();
        let t2 = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            t2.cancel();
        });
        let out = gate
            .acquire_or_wait(RequestClass::Api, Some("user_by_screen_name"), &token)
            .await;
        assert!(matches!(out, Err(XError::Cancelled)));
    }
}
