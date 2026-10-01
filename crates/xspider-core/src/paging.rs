//! 分页原语。**这一小块被两个组件共用，所以放在内核里**：
//! 取数组件用它判断"到底了没有"，下载组件的爬取循环用它决定 `done_reason`。
//! 各写一遍是 `docs/01-ARCHITECTURE.md` §1 明确要避免的事。
//!
//! 契约层面（`docs/01` §3 硬性设计 1）：**只给「单页 + 游标」，不给自动翻页的流**。
//! 翻页时机、过滤、去重是业务语义，必须让调用方看得见游标。这里提供的都是
//! 单页语义的零件，不是一个会自己跑的循环。

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// 一页结果。所有分页端点都返回这个形状。
///
/// - `cursor`：下一页要传的游标；`None` 表示**服务端没有更多了**；
/// - `end`：`cursor.is_none()` 的等价物，显式给出来，免得每个调用方都写一遍
///   （而且它是外壳文案与"下次要不要续爬"的依据，值得显式化）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    pub end: bool,
}

impl<T> Page<T> {
    /// 由条目与游标构造。`end` 由游标推导——**"解析出 0 条"必须表现为 `cursor: None`**
    /// （`docs/02` §B4），这是到底信号的唯一来源。
    pub fn new(items: Vec<T>, cursor: Option<String>) -> Self {
        Self {
            end: cursor.is_none(),
            items,
            cursor,
        }
    }

    /// 空页（`docs/02` §B4：解析出 0 条 → cursor 必须为 null）。
    pub fn empty() -> Self {
        Self {
            items: Vec::new(),
            cursor: None,
            end: true,
        }
    }

    pub fn map<U>(self, f: impl FnMut(T) -> U) -> Page<U> {
        Page {
            items: self.items.into_iter().map(f).collect(),
            cursor: self.cursor,
            end: self.end,
        }
    }
}

/// 游标推进的三种结局。**这是"到底"的唯一判据来源**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorOutcome {
    /// 游标推进了，可以继续翻。
    Advanced,
    /// 服务端说没有更多了（`cursor` 为 `None`）→ `done_reason = exhausted`。
    Exhausted,
    /// 服务端回吐了**和上一页相同的**游标 → 当作到底，`done_reason = cursor_stuck`。
    ///
    /// X 会偶发这种情况（限流或游标失效）。不判停就会原地空转、
    /// 每页都拿同一批数据，**把配额刷爆**（`docs/02` §B8）。
    /// 注意：不要用"页数上限"兜底——那会在正常的长时间线里提前截断。
    Stuck,
}

/// 比较上一页游标与这一页返回的游标。
///
/// `previous` 是**本次请求传出去的**游标（首页为 `None`）；`next` 是响应里给的游标。
pub fn classify_cursor(previous: Option<&str>, next: Option<&str>) -> CursorOutcome {
    match next {
        None => CursorOutcome::Exhausted,
        Some(next) => match previous {
            Some(prev) if prev == next => CursorOutcome::Stuck,
            _ => CursorOutcome::Advanced,
        },
    }
}

/// 跨页去重的 id 集合。
///
/// 为什么是原语：重复 id 在两个组件里都会真的出事——
/// 取数侧表现为列表 id 重复导致渲染错乱（SwiftUI 里的**空白卡片**，
/// `docs/02` §C3）；下载侧会让任务映射错位。各写一遍迟早写歪。
///
/// 纪律（`docs/02` §D1）：**去重必须先于筛选**。
#[derive(Debug, Default, Clone)]
pub struct SeenIds {
    seen: HashSet<String>,
}

impl SeenIds {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(n: usize) -> Self {
        Self {
            seen: HashSet::with_capacity(n),
        }
    }

    /// 首次见到返回 `true`（应保留），重复返回 `false`（应丢弃）。
    pub fn insert(&mut self, id: &str) -> bool {
        self.seen.insert(id.to_string())
    }

    pub fn contains(&self, id: &str) -> bool {
        self.seen.contains(id)
    }

    pub fn len(&self) -> usize {
        self.seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }

    /// 按顺序过滤：保留首次出现的，丢弃重复的。**顺序不变**（列表顺序是语义）。
    pub fn retain_first<T>(&mut self, items: Vec<T>, key: impl Fn(&T) -> Option<String>) -> Vec<T> {
        items
            .into_iter()
            .filter(|item| match key(item) {
                // 拿不到 id 的条目**不能丢**：丢掉等于静默丢数据
                // （与 `docs/02` §D4「无 createdAt 的条目要放行」同一个原则）
                None => true,
                Some(id) => self.insert(&id),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_end_is_derived_from_cursor() {
        let p = Page::new(vec![1, 2, 3], Some("c1".into()));
        assert!(!p.end);
        let p = Page::new(vec![1, 2, 3], None);
        assert!(p.end, "有数据但没有游标，也说明到底了");
        let p = Page::<i32>::empty();
        assert!(p.end && p.cursor.is_none() && p.items.is_empty());
    }

    #[test]
    fn empty_page_must_not_carry_a_cursor() {
        // docs/02 §B4 的机制：解析出 0 条 → cursor 必须为 null。
        // 这条测试防止有人"顺手"把上一页的游标原样传回去（那会导致无限重抓同一页）。
        let p = Page::<serde_json::Value>::empty();
        let json = serde_json::to_value(&p).unwrap();
        assert_eq!(json["end"], true);
        assert!(json.get("cursor").is_none(), "空页不该带 cursor：{json}");
    }

    #[test]
    fn cursor_classification() {
        assert_eq!(
            classify_cursor(Some("c1"), Some("c2")),
            CursorOutcome::Advanced
        );
        assert_eq!(classify_cursor(None, Some("c1")), CursorOutcome::Advanced);
        assert_eq!(classify_cursor(Some("c1"), None), CursorOutcome::Exhausted);
        assert_eq!(classify_cursor(None, None), CursorOutcome::Exhausted);
        // 回吐同一个游标 → 判停，防原地空转刷爆配额
        assert_eq!(
            classify_cursor(Some("same"), Some("same")),
            CursorOutcome::Stuck
        );
    }

    #[test]
    fn seen_ids_keeps_first_occurrence_only() {
        let mut seen = SeenIds::new();
        assert!(seen.insert("a"));
        assert!(!seen.insert("a"), "第二次插入必须是重复");
        assert!(seen.insert("b"));
        assert_eq!(seen.len(), 2);
        assert!(seen.contains("a"));
    }

    #[test]
    fn retain_first_preserves_order_and_drops_duplicates() {
        let mut seen = SeenIds::new();
        let items = vec!["a", "b", "a", "c", "b"];
        let kept = seen.retain_first(items, |s| Some((*s).to_string()));
        assert_eq!(kept, vec!["a", "b", "c"], "顺序必须保持，只去掉重复");
    }

    #[test]
    fn retain_first_never_drops_items_without_an_id() {
        // 与 docs/02 §D4 同一条原则：解析不到 key 的条目要放行，不能静默丢数据
        let mut seen = SeenIds::new();
        let items = vec![Some("a"), None, Some("a"), None];
        let kept = seen.retain_first(items, |v| v.map(str::to_string));
        assert_eq!(kept, vec![Some("a"), None, None]);
    }

    #[test]
    fn page_serializes_without_cursor_key_when_absent() {
        let p = Page::new(vec!["x"], None);
        let json = serde_json::to_value(&p).unwrap();
        assert_eq!(json["items"][0], "x");
        assert_eq!(json["end"], true);
        assert!(json.get("cursor").is_none(), "契约里 cursor 缺省就不该出现");
    }

    #[test]
    fn page_map_preserves_paging_fields() {
        let p = Page::new(vec![1, 2], Some("c".into()));
        let mapped = p.map(|n| n * 10);
        assert_eq!(mapped.items, vec![10, 20]);
        assert_eq!(mapped.cursor.as_deref(), Some("c"));
        assert!(!mapped.end);
    }
}
