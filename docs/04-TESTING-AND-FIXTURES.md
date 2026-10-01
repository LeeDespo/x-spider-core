# 04 · 测试策略与 fixture 纪律

> 目标：**X 改版时你先知道，而不是用户先知道**；以及**换组件时不靠人工回归**。
> 这两件事都不依赖架构，依赖测试。所以它排在最前面做。

---

## 1. 五层测试（从下往上，越往下越必须）

| 层 | 内容 | 网络 | 何时跑 |
|---|---|---|---|
| 1. 单元 | 解析、日期边界、模板展开、令牌桶、游标推进、去重 | ❌ | 每次提交 |
| 2. **fixture 回放** | 真实响应的解析与分页语义 | ❌ | 每次提交 |
| 3. 契约测试 | 同一套 JSON 用例 → 库形态与 sidecar 形态**行为一致** | ❌ | 每次提交 |
| 4. 下载 E2E | 本地 HTTP fixture server：Range、断流、慢速、404、超大 | ❌（本地回环） | 每次提交 |
| 5. **live canary** | 对真 X 各端点取 1 条并断言解析成功 | ✅ | `XSPIDER_LIVE=1` 时手动/定时 |

**默认 `cargo test` 不许碰真网络。** 唯一的例外是第 5 层，且必须 env 门控 + `#[ignore]`。

---

## 2. fixture：本仓库最重要的测试资产

### 2.1 铁律
- **fixture 必须是真实抓到的响应**，不许手写 JSON 冒充。
  手写的样本只能证明"你按自己以为的格式解析正确"，X 改字段时它不会红——
  这正是"用户先发现问题"的根因。
- 采集方式：给组件加一个**录制模式**（`XSPIDER_RECORD=1` 或 `--record-dir`），
  把原始响应按 `endpoint/scenario/日期.json` 落盘；录完再脱敏入库。
- **脱敏规则**（保留结构、去掉个人数据与体积）：
  - `cookie` / `ct0` / token：整段替换为 `"REDACTED"`；
  - 数组裁到 **2–3 条**（保持分页字段 `cursor` 原样，它才是语义关键）；
  - 用户名/昵称/正文：替换为固定假值（保留字符类型与近似的长度）；
  - 媒体 URL：保留 host，路径换成假名（**不要把真实 URL 提交进仓库**）；
  - 不要删字段——删字段等于降低解析覆盖率。

### 2.2 目录与命名
```
fixtures/
├── user_medias/
│   ├── page1_normal.json
│   ├── page2_empty_end.json          # 解析出 0 条 → cursor 必须为 null
│   ├── rate_limited_429.json
│   ├── not_found_404.json
│   └── field_changed_2026_09.json    # 记录一次真实改版的样本（最值钱）
├── tweet_detail/
│   ├── with_promoted_ads.json        # 必须断言广告被过滤干净
│   ├── reply_tree_partial_parent.json
│   └── repeated_retweet_same_id.json # 必须断言去重
└── search_timeline/…
```

### 2.3 每个端点至少覆盖 5 类样本
`正常` / `空页（到底）` / `限流 429` / `404 或未授权` / `字段缺失或改名（改版）`。

---

## 3. 要测的"语义"，不只是"函数"

这些是历史上真的踩过的坑，每条都该有一个测试钉住（对应 `02-X-DOMAIN-NOTES.md`）：

| 断言 | 为什么 |
|---|---|
| 首页请求的 `variables` **不含 cursor 键**（不是 null） | 传 null 会反复请求第一页 |
| 翻页时 `cursor` 用上一页返回值 | — |
| 解析出 0 条 → `cursor: null` | 到底信号 |
| 广告条目被过滤（三个解析入口各一条用例） | 漏一个入口就漏广告 |
| 同一 ID 重复出现 → 去重后唯一 | 重复转推导致空白卡片 |
| 孤儿的 `is_partial_parent = true` 且**不被丢弃** | 父不在本页时不能丢数据 |
| 无 `createdAt` 的条目**放行** | 不能因解析不到时间丢内容 |
| `until:` 是"结束日 + 1 天" | 排他语义；否则"至"当天没内容 |
| 日期用**本地时区**格式化 | UTC 会让筛选差一天 |
| 去重发生在筛选**之前** | 否则不同来源的重复项会漏网 |
| 客户端筛选的终止判据是"时间轴推进" | 停更账号的空窗期不能被判成"没有内容" |
| `wanted_keys` 收齐 → `done_reason = wanted_collected` | 提前终止必须能被调用方识别 |
| 游标未推进 → `done_reason = cursor_stuck` | 防原地空转刷爆配额 |

---

## 4. 契约测试（双形态一致）

一份用例、两个目标：

```rust
// 同一组 (method, json_in, 断言) 分别打到：
// 1) 库形态：直接调 xspider_call
// 2) sidecar 形态：起进程 → HTTP JSON-RPC 调同一 method
fn assert_same_behavior(case: Case) { ... }
```

**不一致即缺陷**（不是"次要偏差"）。同时覆盖：
- `xspider_version()` 握手与版本不匹配时的行为；
- 未知 method → 结构化错误（不是 panic、不是空字符串）；
- 错误 JSON 的形状（见 `01-ARCHITECTURE.md` §6）；
- 取消传播：外壳取消后，Rust 侧真的停了（断言无残留请求/子进程）。

---

## 5. 下载侧测试（不联网也能测全套）

用**本地 HTTP server**（`axum`/`tiny_http` 均可）提供 fixture 字节，覆盖：

| 场景 | 断言 |
|---|---|
| 正常 200 + `Content-Length` | 落盘字节数 == `expect_size`，校验通过 |
| 支持 `Range` 的 206 | 分片下载后拼接内容正确；断点续传从正确偏移开始 |
| 中途断流（服务端主动断开） | 重试退避生效；最终文件完整；不会产出半截文件 |
| `404` / 403 | `failed{reason: not_found/auth_required}`，不重试无意义的请求 |
| 服务端声称大小与实际不符 | 判为 `integrity_failed`，**不是** `completed` |
| 慢速（限速） | 进度事件单调递增；暂停后 `resume` 从断点继续 |
| 并发 5 任务 | 活跃数不超过配置上限 |
| `cancel` | 无残留子进程与临时文件（`pgrep` + 目录扫描断言） |
| 磁盘写满（可用容器/配额模拟，或注入故障） | `failed{reason: disk_full}`，且不破坏已有文件 |

**另测**：`dl.enqueue` 同 `job_id` 两次 → 只算一个任务（幂等）；
重启后 `dl.list()` 能恢复未完成任务并续传。

---

## 6. live canary：你的"X 又变了"报警器

```rust
#[test]
#[ignore]                                   // 默认不跑
fn canary_all_endpoints() {                 // XSPIDER_LIVE=1 cargo test -- --ignored canary
    // 对 6 个端点各取 1 条；断言：HTTP 成功 + 解析成功 + 关键字段非空
    // 失败时输出可读报告：端点 / 阶段（请求/解析）/ 缺失字段 / 原始响应片段
}
```

要求：
- **报告要能一眼定位**："`fetch.user_medias` 解析失败：`entries[0].content.itemContent` 缺失"
  ——不要只报 `assertion failed`；
- 不要在 CI 默认跑（会消耗账号配额）；用**手动触发**或**每日定时**；
- canary 红了 → 立刻把原始响应存成 `field_changed_<日期>.json` 进 fixtures，再修解析。

---

## 7. 反模式（禁止）

- ❌ 手写 JSON 冒充真实响应（无法发现 X 改版）。
- ❌ 默认测试里访问真网络（不稳定、消耗配额、CI 会红得莫名其妙）。
- ❌ 用**错误文案字符串**做断言或逻辑判断。
- ❌ 只测 happy path。
- ❌ 为了"测试通过"去改断言，而不是查清 X 的真实行为。
- ❌ 在 fixture 里提交真实媒体 URL、cookie、他人隐私数据。

---

## 8. 质量门（Definition of Done）

一个功能只有同时满足以下条件才算完成：

1. 契约（`CONTRACT.md` + JSON Schema）已更新；
2. 有真实响应 fixture 的回放测试，覆盖正常 + 至少 2 类异常；
3. `cargo test` 离线全绿；`cargo clippy -- -D warnings` 干净；`cargo fmt --check` 通过；
4. 双形态契约测试一致；
5. 涉及下载的：本地 HTTP E2E 覆盖断点续传与完整性；
6. 新增的坑写进了 `AGENTS.md` 踩坑记录或 `docs/02`。
