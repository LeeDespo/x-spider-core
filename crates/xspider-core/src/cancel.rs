//! 取消传播：外壳取消 → 真的中止在飞的请求（`docs/01-ARCHITECTURE.md` §3 硬性设计 3）。
//!
//! 实现要点：用 `tokio::select!` 把「在飞的 future」与「取消信号」竞速。
//! 取消时不是设个标志位等下次检查，而是**直接 drop 掉 future**——
//! 连接随之关闭，不会留下一个还在跑的空转请求（那正是限流风暴的放大器）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::Notify;

use crate::error::{XError, XResult};

#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    inner: Arc<CancelInner>,
}

#[derive(Debug, Default)]
struct CancelInner {
    flag: AtomicBool,
    notify: Notify,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.inner.flag.store(true, Ordering::SeqCst);
        // notify_waiters（而不是 notify_one）：可能有多个在飞请求同时等待取消
        self.inner.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.flag.load(Ordering::SeqCst)
    }

    /// 一直等到取消。已取消时立即返回。
    pub async fn cancelled(&self) {
        loop {
            if self.is_cancelled() {
                return;
            }
            // 先注册再复查，堵住「检查之后、注册之前」的竞态窗口
            let notified = self.inner.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }

    /// 竞速辅助：取消先到则返回 `Cancelled`，并 drop 掉原 future。
    pub async fn race<T>(&self, fut: impl std::future::Future<Output = T>) -> XResult<T> {
        if self.is_cancelled() {
            return Err(XError::Cancelled);
        }
        tokio::select! {
            biased;
            _ = self.cancelled() => Err(XError::Cancelled),
            out = fut => Ok(out),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn cancelled_returns_immediately_when_already_cancelled() {
        let token = CancelToken::new();
        token.cancel();
        tokio::time::timeout(Duration::from_millis(50), token.cancelled())
            .await
            .expect("已取消的 token 必须立即返回");
    }

    #[tokio::test]
    async fn race_aborts_a_slow_future() {
        let token = CancelToken::new();
        let t2 = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            t2.cancel();
        });
        let out = token
            .race(async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                "不该到这里"
            })
            .await;
        assert!(matches!(out, Err(XError::Cancelled)));
    }

    #[tokio::test]
    async fn race_passes_through_when_not_cancelled() {
        let token = CancelToken::new();
        assert_eq!(token.race(async { 7 }).await.unwrap(), 7);
    }
}
