//! **爬取调度**：候选清单 + 策略参数 + `done_reason`（`docs/01-ARCHITECTURE.md` §4）。
//!
//! # 为什么这一层单独存在
//!
//! 爬虫的难点不在下载字节，而在**策略**：什么时候停、什么算重复、边界日怎么算。
//! 上游与移植版在这里踩过的坑最多（`docs/02` §D 整节），所以它必须一次写对，
//! 而且要能被离线测试逐个钉住——所以它接受一个**注入的取页函数**，
//! 于是测试可以喂任意脚本化的"服务端"，不需要网络。
//!
//! # 九条纪律（每条都有对应测试）
//!
//! | 纪律 | 出处 |
//! |---|---|
//! | **游标先推进，再过滤** | `docs/02` §B3（否则被筛空的页会被无限重抓） |
//! | **去重先于筛选** | §D1 |
//! | **主终止判据是"时间轴推进"，不是空页计数** | §D2（停更一两个月的账号会被误判成"没有内容"） |
//! | 空页计数只能当**辅助**上限 | §D2 |
//! | **无 `createdAt` 的条目放行** | §D4 |
//! | **到底只看服务端原始条数**，不看筛选后的 | §D5 |
//! | **游标没推进就判停**（不当页数上限） | §B8 |
//! | `until` 的边界要**整天包含** | §D3 |
//! | 页间节流（不做 429 风暴的放大器） | §B1 / §B2 |
//!
//! # 与下载组件的关系
//!
//! 爬取只产出**候选清单**。要不要下、叫什么名、放哪儿由外壳决定
//! （`docs/01` §4）。所以这里**不依赖下载组件**——两个组件不直接对接。

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use xspider_core::cancel::CancelToken;
use xspider_core::error::{XError, XResult};
use xspider_core::paging::{classify_cursor, CursorOutcome, Page, SeenIds};

pub use crate::DoneReason;

/// 媒体类型（与取数组件的 `MediaKind` 同形，避免两个组件互相依赖类型）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Photo,
    Video,
    AnimatedGif,
}

/// 一条候选媒体。外壳拿它去决定"下不下、叫什么名、放哪儿"。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    /// 去重键。**默认是媒体 id**：同一张图在两条推文里出现只算一次。
    pub key: String,
    pub post_id: String,
    pub media_id: String,
    pub kind: MediaKind,
    pub url: String,
    pub ext: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_hint: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screen_name: Option<String>,
    /// 推文所在日期（`YYYY-MM-DD`，**UTC**）。外壳按天分目录时用它。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub day: Option<String>,
}

/// 策略参数（对应契约里的 `crawl.start`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrawlStrategy {
    /// 起始日（含当天），`YYYY-MM-DD`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    /// 结束日（**含当天**），`YYYY-MM-DD`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    /// 只要这些媒体类型；缺省表示不按类型筛。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_types: Option<Vec<MediaKind>>,
    /// 收齐这些 key 就停。
    ///
    /// **它进契约**（`docs/01` §4 要点 2）：它改变**翻页终止条件**（收齐即停 = 省请求），
    /// 是成本相关的；而"排除某些 key"只是产品语义，外壳在候选清单上过滤即可，零额外请求。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wanted_keys: Option<Vec<String>>,
    #[serde(default)]
    pub limits: CrawlLimits,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrawlLimits {
    /// 每页请求多少条（透传给端点）。
    pub page_size: u64,
    /// **页间节流**（毫秒）。上游靠浏览器渲染节奏天然限速，Rust 循环没有，必须显式等价。
    #[serde(default = "default_throttle")]
    pub page_throttle_ms: u64,
    /// 翻页数上限（**辅助**，不是主判据）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_pages: Option<u32>,
    /// 连续空页上限（**辅助**）。默认 5：`docs/02` §D2 说停更账号的空窗期
    /// 会被这个判据误伤，所以它只能当上限，不能当主判据。
    #[serde(default = "default_empty_page_limit")]
    pub empty_page_limit: u32,
    /// 只要比这一天新的内容（`YYYY-MM-DD`）。比 `since` 更严格的一道闸。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_when_older_than: Option<String>,
}

fn default_throttle() -> u64 {
    450
}
fn default_empty_page_limit() -> u32 {
    5
}

impl Default for CrawlLimits {
    fn default() -> Self {
        Self {
            page_size: 20,
            page_throttle_ms: default_throttle(),
            max_pages: None,
            empty_page_limit: default_empty_page_limit(),
            stop_when_older_than: None,
        }
    }
}

/// 一次爬取的结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrawlOutcome {
    /// **为什么停的**。外壳靠它决定文案与"下次是否续爬"（`docs/01` §4 要点 1）。
    pub done_reason: DoneReason,
    pub candidates: Vec<Candidate>,
    /// 翻了几页（含空页）。
    pub pages: u32,
    /// 服务端给的原始条目数（**不是**筛选后的），到底判据只看它。
    pub raw_items: u64,
    /// 去重/筛选各丢了多少（排查"为什么这次没东西"用）。
    pub dropped: CrawlStats,
    /// 下一次从哪儿继续（非 `Exhausted` 时有用）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CrawlStats {
    pub dropped_duplicate: u64,
    pub dropped_by_date: u64,
    pub dropped_by_type: u64,
    pub dropped_no_media: u64,
    pub dropped_name: u64,
}

/// 爬取过程中发出的事件（**按页批量**，不逐条回调外壳：跨边界次数不可控）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CrawlEvent {
    Page {
        index: u32,
        raw_count: u64,
        kept_count: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        oldest_at: Option<String>,
    },
    Candidates {
        index: u32,
        items: Vec<Candidate>,
    },
    Done {
        reason: DoneReason,
    },
}

/// 取一页。**注入点**：生产环境是 fetch 组件的端点，测试里是脚本化的假服务端。
pub type PageSource = Arc<
    dyn Fn(
            Option<String>,
        )
            -> std::pin::Pin<Box<dyn std::future::Future<Output = XResult<Page<CrawlPost>>> + Send>>
        + Send
        + Sync,
>;

/// 爬取循环需要的最小推文形状。
///
/// **刻意不复用取数组件的 `Post`**：爬取只需要"时间 + 媒体 + 作者"，
/// 而两个组件之间不许直接依赖（`docs/01` §1）。外壳把端点的结果映射成这个形状即可。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrawlPost {
    pub id: String,
    /// RFC3339 UTC。`None` = X 没给时间，**必须放行**（`docs/02` §D4）。
    pub created_at: Option<String>,
    pub screen_name: Option<String>,
    pub medias: Vec<CrawlMedia>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrawlMedia {
    pub id: String,
    pub kind: MediaKind,
    pub url: String,
    pub ext: String,
    pub size_hint: Option<u64>,
}

/// 一次爬取。全程可取消、可观察（事件回调）。
///
/// `emit` 是**同步**回调：它只负责把事件交出去（写通道、记日志），
/// 不该在里面做 IO 等待——否则页间节流会被它拖偏。
pub async fn crawl(
    strategy: &CrawlStrategy,
    source: PageSource,
    cancel: &CancelToken,
    mut emit: impl FnMut(CrawlEvent),
) -> XResult<CrawlOutcome> {
    let mut cursor: Option<String> = None;
    let mut seen_posts = SeenIds::new();
    let mut seen_keys: BTreeSet<String> = BTreeSet::new();
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut stats = CrawlStats::default();
    let mut pages = 0u32;
    let mut raw_items = 0u64;
    let mut empty_pages = 0u32;
    let mut oldest_seen: Option<String> = None;

    let wanted: BTreeSet<&String> = strategy
        .wanted_keys
        .as_ref()
        .map(|keys| keys.iter().collect())
        .unwrap_or_default();
    let done_reason;

    loop {
        if cancel.is_cancelled() {
            done_reason = DoneReason::Cancelled;
            break;
        }
        // 页数上限是**辅助**判据：先用它挡住失控，再看真正的原因
        if let Some(max_pages) = strategy.limits.max_pages {
            if pages >= max_pages {
                done_reason = DoneReason::PageLimitReached;
                break;
            }
        }

        let page = source(cursor.clone())
            .await
            .map_err(|e| XError::internal(format!("取页失败（第 {} 页）：{e}", pages + 1)))?;

        pages += 1;
        raw_items += page.items.len() as u64;

        // ---- ① 先推进游标（docs/02 §B3）----
        // 顺序在这里是硬性的：如果先过滤再推进，被筛空的页会让游标原地不动，
        // 于是无限重抓同一页（配额风暴）。
        let outcome = classify_cursor(cursor.as_deref(), page.cursor.as_deref());
        let next_cursor = page.cursor.clone();
        cursor = next_cursor.clone();

        // ---- ② 去重（在筛选之前，docs/02 §D1）----
        let page_ids: Vec<String> = {
            let mut ids = Vec::new();
            for post in &page.items {
                if seen_posts.insert(&post.id) {
                    ids.push(post.id.clone());
                } else {
                    stats.dropped_duplicate += 1;
                }
            }
            ids
        };

        let mut kept = 0u64;
        let mut page_candidates: Vec<Candidate> = Vec::new();
        for post in &page.items {
            if !page_ids.contains(&post.id) {
                continue;
            }
            // 时间轴推进（主判据，docs/02 §D2）
            if let Some(created) = &post.created_at {
                // 记录**最旧**的那条（RFC3339 的字典序与时间序一致）
                if oldest_seen
                    .as_deref()
                    .map(|old| created.as_str() < old)
                    .unwrap_or(true)
                {
                    oldest_seen = Some(created.clone());
                }
            }
            if keep_post(post, strategy, &mut stats) {
                kept += 1;
                for media in &post.medias {
                    if !seen_keys.insert(media.id.clone()) {
                        stats.dropped_duplicate += 1;
                        continue;
                    }
                    page_candidates.push(Candidate {
                        key: media.id.clone(),
                        post_id: post.id.clone(),
                        media_id: media.id.clone(),
                        kind: media.kind,
                        url: media.url.clone(),
                        ext: media.ext.clone(),
                        size_hint: media.size_hint,
                        created_at: post.created_at.clone(),
                        screen_name: post.screen_name.clone(),
                        day: post.created_at.as_deref().map(date_of),
                    });
                }
            }
        }

        emit(CrawlEvent::Page {
            index: pages,
            raw_count: page.items.len() as u64,
            kept_count: kept,
            cursor: next_cursor.clone(),
            oldest_at: oldest_seen.clone(),
        });
        if !page_candidates.is_empty() {
            candidates.extend(page_candidates.iter().cloned());
            emit(CrawlEvent::Candidates {
                index: pages,
                items: page_candidates,
            });
        }

        // ---- ③ 终止判据（顺序即优先级）----

        // 收齐 wanted → 提前停（省请求）
        if !wanted.is_empty()
            && wanted
                .iter()
                .all(|key| candidates.iter().any(|c| &c.key == *key))
        {
            done_reason = DoneReason::WantedCollected;
            break;
        }

        // 客户端筛选的时间轴推进到了 `since` 之前（**主判据**）
        if let Some(since) = &strategy.since {
            if oldest_seen
                .as_deref()
                .map(|oldest| date_of(oldest) < *since)
                .unwrap_or(false)
            {
                done_reason = DoneReason::TimeProgressed;
                break;
            }
        }
        if let Some(bound) = &strategy.limits.stop_when_older_than {
            if oldest_seen
                .as_deref()
                .map(|oldest| date_of(oldest) < *bound)
                .unwrap_or(false)
            {
                done_reason = DoneReason::TimeProgressed;
                break;
            }
        }

        // 到底 / 游标没推进
        match outcome {
            CursorOutcome::Exhausted => {
                // **到底只看服务端有没有给游标**，不看筛选后的条数（docs/02 §D5）
                done_reason = DoneReason::Exhausted;
                break;
            }
            CursorOutcome::Stuck => {
                done_reason = DoneReason::CursorStuck;
                break;
            }
            CursorOutcome::Advanced => {}
        }

        // 空页计数（**辅助**上限）
        if page.items.is_empty() {
            empty_pages += 1;
            if empty_pages >= strategy.limits.empty_page_limit {
                done_reason = DoneReason::EmptyPages;
                break;
            }
        } else {
            empty_pages = 0;
        }

        // ---- ④ 页间节流（不做 429 风暴的放大器，docs/02 §B1/B2）----
        if strategy.limits.page_throttle_ms > 0 {
            let wait = Duration::from_millis(strategy.limits.page_throttle_ms);
            match cancel.race(tokio::time::sleep(wait)).await {
                Ok(()) => {}
                Err(_) => {
                    done_reason = DoneReason::Cancelled;
                    break;
                }
            }
        }
    }

    emit(CrawlEvent::Done {
        reason: done_reason,
    });

    Ok(CrawlOutcome {
        done_reason,
        candidates,
        pages,
        raw_items,
        dropped: stats,
        next_cursor: cursor,
    })
}

/// 一条推文要不要保留。**只做筛选，不做去重**（去重已经在前面做过）。
fn keep_post(post: &CrawlPost, strategy: &CrawlStrategy, stats: &mut CrawlStats) -> bool {
    // 无 created_at 的条目**放行**（docs/02 §D4：不能因为解析不到时间就丢内容）
    if let Some(created) = &post.created_at {
        let day = date_of(created);
        if let Some(since) = &strategy.since {
            if day < *since {
                stats.dropped_by_date += 1;
                return false;
            }
        }
        // `until` **含当天**（docs/02 §D3）——所以比较用 `<=`
        if let Some(until) = &strategy.until {
            if day > *until {
                stats.dropped_by_date += 1;
                return false;
            }
        }
    }
    if post.medias.is_empty() {
        // 纯文字推文：不受"媒体类型筛选"影响，但也没有候选可产出
        stats.dropped_no_media += 1;
        return false;
    }
    if let Some(types) = &strategy.media_types {
        let has = post.medias.iter().any(|m| types.contains(&m.kind));
        if !has {
            stats.dropped_by_type += 1;
            return false;
        }
    }
    true
}

/// RFC3339 里的日期部分（UTC）。
///
/// 注意口径：客户端筛选按 **UTC 日期**比较。所以**边界日**可能与用户本地日历差一天
/// （跨零点的推文）。要精确到本地日历，外壳就该把 `since`/`until` 也按本地日历给出，
/// 并接受这一天之内的偏移——`docs/02` §D3 那条"用本地时区格式化"说的是**请求参数**，
/// 组件已经在 `fetch.search_timeline` 里按日历日期处理（不做时区换算）。
fn date_of(rfc3339: &str) -> String {
    rfc3339.chars().take(10).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// 脚本化的假服务端：按顺序吐出准备好的页。
    ///
    /// 用它而不是真网络：爬取逻辑的每一条纪律都是"纯策略"，
    /// 用真实响应反而看不清是哪一条在起作用。
    fn scripted_source(
        pages: Vec<Page<CrawlPost>>,
    ) -> (PageSource, Arc<Mutex<Vec<Option<String>>>>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let pages = Arc::new(Mutex::new(pages.into_iter()));
        let calls_clone = calls.clone();
        let source: PageSource = Arc::new(move |cursor: Option<String>| {
            calls_clone.lock().unwrap().push(cursor);
            let pages = pages.clone();
            Box::pin(async move {
                let next = pages.lock().unwrap().next();
                next.ok_or_else(|| XError::internal("脚本里没有更多页了（说明多翻了一页）"))
            })
        });
        (source, calls)
    }

    fn media(id: &str) -> CrawlMedia {
        CrawlMedia {
            id: id.to_string(),
            kind: MediaKind::Photo,
            url: format!("https://pbs.twimg.com/media/{id}.jpg"),
            ext: "jpg".into(),
            size_hint: Some(1000),
        }
    }

    fn post(id: &str, created: Option<&str>, media_ids: &[&str]) -> CrawlPost {
        CrawlPost {
            id: id.to_string(),
            created_at: created.map(str::to_string),
            screen_name: Some("demo_user".into()),
            medias: media_ids.iter().map(|m| media(m)).collect(),
        }
    }

    fn strategy() -> CrawlStrategy {
        CrawlStrategy {
            since: None,
            until: None,
            media_types: None,
            wanted_keys: None,
            limits: CrawlLimits {
                // 测试里不要真的等：节流本身单有一条测试
                page_throttle_ms: 0,
                ..CrawlLimits::default()
            },
        }
    }

    async fn run(pages: Vec<Page<CrawlPost>>, strategy: CrawlStrategy) -> CrawlOutcome {
        let (source, _) = scripted_source(pages);
        crawl(&strategy, source, &CancelToken::new(), |_| {})
            .await
            .expect("爬取不该失败")
    }

    #[tokio::test]
    async fn stops_with_exhausted_when_the_server_has_no_more() {
        let pages = vec![
            Page::new(
                vec![post("1", Some("2026-09-30T10:00:00Z"), &["m1"])],
                Some("c1".into()),
            ),
            Page::new(vec![post("2", Some("2026-09-29T10:00:00Z"), &["m2"])], None),
        ];
        let outcome = run(pages, strategy()).await;
        assert_eq!(outcome.done_reason, DoneReason::Exhausted);
        assert_eq!(outcome.pages, 2);
        assert_eq!(outcome.candidates.len(), 2);
        assert_eq!(outcome.raw_items, 2);
    }

    /// **游标没推进就判停**（docs/02 §B8）：否则会原地空转把配额刷爆。
    #[tokio::test]
    async fn stops_with_cursor_stuck_when_the_server_repeats_the_cursor() {
        let pages = vec![
            Page::new(
                vec![post("1", Some("2026-09-30T10:00:00Z"), &["m1"])],
                Some("same".into()),
            ),
            // 回吐同一个游标
            Page::new(
                vec![post("2", Some("2026-09-29T10:00:00Z"), &["m2"])],
                Some("same".into()),
            ),
        ];
        let outcome = run(pages, strategy()).await;
        assert_eq!(outcome.done_reason, DoneReason::CursorStuck);
        assert_eq!(outcome.pages, 2, "第二页就该判停，不能再翻");
    }

    #[tokio::test]
    async fn stops_with_empty_pages_at_the_auxiliary_limit() {
        let limits = CrawlLimits {
            empty_page_limit: 2,
            page_throttle_ms: 0,
            ..CrawlLimits::default()
        };
        let pages = vec![
            Page::new(vec![], Some("c2".into())),
            Page::new(vec![], Some("c3".into())),
            Page::new(vec![post("9", Some("2026-09-01T00:00:00Z"), &["m9"])], None),
        ];
        let outcome = run(
            pages,
            CrawlStrategy {
                limits,
                ..strategy()
            },
        )
        .await;
        assert_eq!(outcome.done_reason, DoneReason::EmptyPages);
        assert_eq!(outcome.pages, 2, "空页上限是 2，翻到第二页就该停");
    }

    /// **主判据是时间轴推进**（docs/02 §D2）：停更账号的空窗期不能被判成"没有内容"。
    #[tokio::test]
    async fn time_progression_is_the_primary_stop_reason() {
        let mut strategy = strategy();
        strategy.since = Some("2026-09-28".to_string());
        // 第一页全是"新"内容，第二页出现早于 since 的 → 停
        let pages = vec![
            Page::new(
                vec![
                    post("1", Some("2026-09-30T10:00:00Z"), &["m1"]),
                    post("2", Some("2026-09-29T10:00:00Z"), &["m2"]),
                ],
                Some("c2".into()),
            ),
            Page::new(
                vec![post("3", Some("2026-09-27T10:00:00Z"), &["m3"])],
                Some("c3".into()),
            ),
        ];
        let outcome = run(pages, strategy).await;
        assert_eq!(outcome.done_reason, DoneReason::TimeProgressed);
        assert_eq!(
            outcome.candidates.len(),
            2,
            "早于 since 的那条不该进候选（但它触发了停止）"
        );
        assert_eq!(outcome.dropped.dropped_by_date, 1);
    }

    /// 停更账号：**连续空窗不能当"没有内容"**——时间轴推进才是判据。
    #[tokio::test]
    async fn a_stale_account_with_no_recent_posts_reports_time_progressed_not_empty_pages() {
        let mut strategy = strategy();
        strategy.since = Some("2026-09-01".to_string());
        // 三页都是老内容，每页都有条目（不是空页）
        let pages = vec![
            Page::new(
                vec![post("1", Some("2026-08-20T10:00:00Z"), &["m1"])],
                Some("c1".into()),
            ),
            Page::new(
                vec![post("2", Some("2026-07-01T10:00:00Z"), &["m2"])],
                Some("c2".into()),
            ),
        ];
        let outcome = run(pages, strategy).await;
        assert_eq!(
            outcome.done_reason,
            DoneReason::TimeProgressed,
            "第一页就已经早于 since，必须立刻按时间判停"
        );
        assert_eq!(outcome.pages, 1);
        assert!(outcome.candidates.is_empty());
    }

    #[tokio::test]
    async fn stops_with_wanted_collected() {
        let mut strategy = strategy();
        strategy.wanted_keys = Some(vec!["m2".to_string()]);
        let pages = vec![
            Page::new(
                vec![post("1", Some("2026-09-30T10:00:00Z"), &["m1"])],
                Some("c2".into()),
            ),
            Page::new(
                vec![post("2", Some("2026-09-29T10:00:00Z"), &["m2"])],
                Some("c3".into()),
            ),
            // 这一页不该被翻到
            Page::new(vec![post("3", Some("2026-09-28T10:00:00Z"), &["m3"])], None),
        ];
        let outcome = run(pages, strategy).await;
        assert_eq!(outcome.done_reason, DoneReason::WantedCollected);
        assert_eq!(outcome.pages, 2, "收齐就该停，不该再翻第三页");
        assert_eq!(outcome.candidates.len(), 2);
    }

    /// **去重先于筛选**（docs/02 §D1）：重复条目被去重丢掉，不会先被日期筛掉。
    #[tokio::test]
    async fn dedup_happens_before_filtering() {
        let mut strategy = strategy();
        strategy.since = Some("2026-09-29".to_string());
        let pages = vec![
            Page::new(
                vec![post("dup", Some("2026-09-30T10:00:00Z"), &["m1"])],
                Some("c2".into()),
            ),
            // 同一个 post id 再次出现，但时间"旧"——去重该先命中，而不是记成"被日期筛掉"
            Page::new(
                vec![post("dup", Some("2026-01-01T10:00:00Z"), &["m9"])],
                None,
            ),
        ];
        let outcome = run(pages, strategy).await;
        assert_eq!(outcome.dropped.dropped_duplicate, 1);
        assert_eq!(
            outcome.dropped.dropped_by_date, 0,
            "重复条目应当在去重那一步就丢了"
        );
        assert_eq!(outcome.candidates.len(), 1, "候选按媒体 id 去重");
    }

    /// 同一张图出现在两条推文里 → 只产出一个候选。
    #[tokio::test]
    async fn the_same_media_in_two_posts_is_one_candidate() {
        let pages = vec![Page::new(
            vec![
                post("1", Some("2026-09-30T10:00:00Z"), &["same"]),
                post("2", Some("2026-09-30T09:00:00Z"), &["same"]),
            ],
            None,
        )];
        let outcome = run(pages, strategy()).await;
        assert_eq!(outcome.candidates.len(), 1);
        assert_eq!(outcome.candidates[0].post_id, "1", "保留首次出现的那条");
    }

    /// 无 `createdAt` 的条目**放行**（docs/02 §D4）。
    #[tokio::test]
    async fn posts_without_created_at_are_kept() {
        let pages = vec![Page::new(vec![post("1", None, &["m1"])], None)];
        let outcome = run(pages, strategy()).await;
        assert_eq!(outcome.candidates.len(), 1);
        assert!(
            outcome.candidates[0].created_at.is_none(),
            "没有时间就是没有，不许编一个"
        );
        assert_eq!(outcome.candidates[0].day, None);
    }

    /// `until` **含当天**（docs/02 §D3）。
    #[tokio::test]
    async fn until_includes_the_whole_end_day() {
        let mut strategy = strategy();
        strategy.until = Some("2026-09-30".to_string());
        let pages = vec![Page::new(
            vec![
                post("in", Some("2026-09-30T23:59:59Z"), &["m1"]),
                post("out", Some("2026-10-01T00:00:01Z"), &["m2"]),
            ],
            None,
        )];
        let outcome = run(pages, strategy).await;
        assert_eq!(outcome.candidates.len(), 1);
        assert_eq!(
            outcome.candidates[0].post_id, "in",
            "结束日当天必须整天包含"
        );
        assert_eq!(outcome.dropped.dropped_by_date, 1);
    }

    #[tokio::test]
    async fn media_type_filter_applies() {
        let mut strategy = strategy();
        strategy.media_types = Some(vec![MediaKind::Video]);
        let mut photo = post("1", Some("2026-09-30T10:00:00Z"), &["m1"]);
        photo.medias[0].kind = MediaKind::Photo;
        let mut video = post("2", Some("2026-09-30T09:00:00Z"), &["m2"]);
        video.medias[0].kind = MediaKind::Video;
        let outcome = run(vec![Page::new(vec![photo, video], None)], strategy).await;
        assert_eq!(outcome.candidates.len(), 1);
        assert_eq!(outcome.candidates[0].kind, MediaKind::Video);
        assert_eq!(outcome.dropped.dropped_by_type, 1);
    }

    /// 翻页时**传出去的必须是上一页返回的游标**（docs/04 §3）。
    #[tokio::test]
    async fn each_page_request_carries_the_previous_cursor() {
        let pages = vec![
            Page::new(
                vec![post("1", Some("2026-09-30T10:00:00Z"), &["m1"])],
                Some("c1".into()),
            ),
            Page::new(
                vec![post("2", Some("2026-09-29T10:00:00Z"), &["m2"])],
                Some("c2".into()),
            ),
            Page::new(vec![post("3", Some("2026-09-28T10:00:00Z"), &["m3"])], None),
        ];
        let (source, calls) = scripted_source(pages);
        let outcome = crawl(&strategy(), source, &CancelToken::new(), |_| {})
            .await
            .unwrap();
        assert_eq!(outcome.done_reason, DoneReason::Exhausted);
        let calls = calls.lock().unwrap().clone();
        assert_eq!(
            calls,
            vec![None, Some("c1".to_string()), Some("c2".to_string())],
            "首页不带游标；之后每页带上一页返回的那个"
        );
    }

    #[tokio::test]
    async fn cancellation_stops_the_crawl() {
        let cancel = CancelToken::new();
        cancel.cancel();
        let (source, calls) = scripted_source(vec![Page::new(
            vec![post("1", Some("2026-09-30T10:00:00Z"), &["m1"])],
            None,
        )]);
        let outcome = crawl(&strategy(), source, &cancel, |_| {}).await.unwrap();
        assert_eq!(outcome.done_reason, DoneReason::Cancelled);
        assert!(calls.lock().unwrap().is_empty(), "取消后一页都不该取");
    }

    #[tokio::test]
    async fn events_are_batched_per_page_and_end_with_done() {
        let pages = vec![
            Page::new(
                vec![post("1", Some("2026-09-30T10:00:00Z"), &["m1"])],
                Some("c1".into()),
            ),
            Page::new(vec![post("2", Some("2026-09-29T10:00:00Z"), &["m2"])], None),
        ];
        let (source, _) = scripted_source(pages);
        let mut events: Vec<CrawlEvent> = Vec::new();
        crawl(&strategy(), source, &CancelToken::new(), |e| events.push(e))
            .await
            .unwrap();

        let page_events: Vec<&CrawlEvent> = events
            .iter()
            .filter(|e| matches!(e, CrawlEvent::Page { .. }))
            .collect();
        assert_eq!(page_events.len(), 2, "每页一条 Page 事件");
        // 候选是**按页批量**给的，不是逐条
        let batches: Vec<usize> = events
            .iter()
            .filter_map(|e| match e {
                CrawlEvent::Candidates { items, .. } => Some(items.len()),
                _ => None,
            })
            .collect();
        assert_eq!(batches, vec![1, 1]);
        assert!(matches!(
            events.last(),
            Some(CrawlEvent::Done {
                reason: DoneReason::Exhausted
            })
        ));
    }

    /// 页间节流必须真的等（`docs/02` §B1：不做 429 风暴的放大器）。
    #[tokio::test]
    async fn page_throttle_is_respected() {
        let pages = vec![
            Page::new(
                vec![post("1", Some("2026-09-30T10:00:00Z"), &["m1"])],
                Some("c1".into()),
            ),
            Page::new(vec![post("2", Some("2026-09-29T10:00:00Z"), &["m2"])], None),
        ];
        let (source, _) = scripted_source(pages);
        let strategy = CrawlStrategy {
            limits: CrawlLimits {
                page_throttle_ms: 120,
                ..CrawlLimits::default()
            },
            ..strategy()
        };
        let started = std::time::Instant::now();
        crawl(&strategy, source, &CancelToken::new(), |_| {})
            .await
            .unwrap();
        assert!(
            started.elapsed() >= Duration::from_millis(120),
            "两页之间必须等够节流时间，实际 {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn max_pages_caps_the_loop() {
        let strategy = CrawlStrategy {
            limits: CrawlLimits {
                max_pages: Some(2),
                page_throttle_ms: 0,
                ..CrawlLimits::default()
            },
            ..strategy()
        };
        let pages = vec![
            Page::new(
                vec![post("1", Some("2026-09-30T10:00:00Z"), &["m1"])],
                Some("c1".into()),
            ),
            Page::new(
                vec![post("2", Some("2026-09-29T10:00:00Z"), &["m2"])],
                Some("c2".into()),
            ),
            Page::new(vec![post("3", Some("2026-09-28T10:00:00Z"), &["m3"])], None),
        ];
        let outcome = run(pages, strategy).await;
        assert_eq!(
            outcome.done_reason,
            DoneReason::PageLimitReached,
            "触到调用方给的上限，理由应当明确"
        );
        assert_eq!(outcome.pages, 2, "上限是辅助判据，先挡住失控");
        assert_eq!(outcome.candidates.len(), 2);
    }

    #[test]
    fn date_of_takes_the_utc_date_part() {
        assert_eq!(date_of("2026-09-30T12:34:56Z"), "2026-09-30");
        assert_eq!(date_of("2026-09-30T12:34:56+08:00"), "2026-09-30");
        // 非法的短串也不能 panic
        assert_eq!(date_of("bad"), "bad");
    }
}
