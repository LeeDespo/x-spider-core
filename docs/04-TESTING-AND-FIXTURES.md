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
- 采集方式：运行 `crates/xspider-fetch/tests/record_live.rs` 中的 `#[ignore]` 录制测试。
  需要显式设 `XSPIDER_LIVE=1`、`XSPIDER_COOKIE`（代理需要时设 `XSPIDER_PROXY`）；原始响应落在
  `fixtures/<endpoint>/raw/<scenario>.json`，再用 `python3 script/redact_fixtures.py` 脱敏入库。
  `record_xclid_page` 单独从真实登录态页面提取签名原料；这两个路径都不是 `XSPIDER_RECORD`
  或 `--record-dir` 开关。
- **脱敏规则**（保留结构、去掉个人数据与体积）：
  - `cookie` / `ct0` / token：整段替换为 `"REDACTED"`；
  - 数组最多留 **3 条**（保持分页字段 `cursor` 原样，它才是语义关键）；
  - 用户名/昵称/正文：替换为固定假值（保留字符类型与近似的长度）；
  - 媒体 URL：保留 host，路径换成假名（**不要把真实 URL 提交进仓库**）；
  - 不要删字段——删字段等于降低解析覆盖率；数字 ID 的假值须互不相同，避免改变去重结果。

### 2.2 当前目录与命名
```
fixtures/
├── user_by_screen_name/{normal,not_found,unauthorized}.json
├── user_medias/{page1,page2}.json
├── user_tweets/page1.json
├── tweet_detail/with_replies.json
├── search_timeline/{media_only,query_id_source}.json
├── home_timeline/{for_you,following}.json
├── following/page1.json
└── xclid/page_artifacts.json         # 签名原料；不是 HTTP response fixture
```

该树是当前已入库的资产，不是每端点都要达到同一组场景。`search_timeline/query_id_source.json`
是 queryId 自愈所用的 bundle 片段，没有 `response` 字段，回放加载器会跳过它。原始录制位于
各端点的 `raw/` 子目录，已由 `.gitignore` 排除且回放加载器跳过；准确覆盖与刻意留空的场景
见 [`../fixtures/README.md`](../fixtures/README.md)。

### 2.3 按真实证据维护覆盖

每个端点不强制收齐固定的五类响应。先入库有代表性的真实正常响应和真实分页形态；异常样本只能
来自实际观察。当前没有真实 429 或字段改名响应，也没有为每个端点各自采集 404/未授权样本。
429 的限流状态机由单元测试覆盖，下载 HTTP 错误由本地 HTTP E2E 覆盖；这两种测试不得标成真实 X
响应 fixture。字段改名只在 canary 真正捕获变化后加入。

---

## 3. 要测的"语义"，不只是"函数"

这些是历史上真的踩过的坑，每条都该有一个测试钉住（对应 `02-X-DOMAIN-NOTES.md`）：

| 断言 | 为什么 |
|---|---|
| 首页请求的 `variables` **不含 cursor 键**（不是 null） | 传 null 会反复请求第一页 |
| 翻页时 `cursor` 用上一页返回值 | — |
| 服务端无下一页 → page DTO 省略 `cursor`；客户端筛空但服务端仍有游标 → 保留游标 | 到底信号与筛选后的空页不能混为一谈 |
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

其中「同一 ID 重复出现 → 去重后唯一」与两行游标断言，曾被 fixture 脱敏事故假红过——
脱敏把不同 id 洗成同一个值、把游标条目的 `value` 洗成固定假值，症状都像"端点逻辑坏了"，
现场案例见 §9 第 17、18 条。

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

**另测**：`dl.enqueue` 同 `job_id` 两次 → 只算一个任务（幂等）；落盘恢复测试覆盖
`waiting/active → waiting`、`paused → paused`、`error → error`，取消造成的 `error` 不会自动重试。
重启后要由外壳用 `dl.list()` 对账；若外壳策略是重启不自动继续，应显式暂停恢复出的 waiting 任务。

---

## 6. live canary：你的"X 又变了"报警器

```rust
#[test]
#[ignore]                                   // 默认不跑
fn canary_all_endpoints() {                 // XSPIDER_LIVE=1 cargo test -- --ignored canary
    // 检查 7 个 fetch method、共 8 次请求；断言：HTTP 成功 + 解析成功 + 关键字段非空
    // 失败时输出可读报告：端点 / 阶段（请求/解析）/ 缺失字段 / 原始响应片段
}
```

要求：
- **报告要能一眼定位**："`fetch.user_medias` 解析失败：`entries[0].content.itemContent` 缺失"
  ——不要只报 `assertion failed`；
- 不要在 CI 默认跑（会消耗账号配额）；用**手动触发**或**每日定时**；
- canary 当前覆盖 `fetch.get_user`、`user_medias`、`user_tweets`、`tweet_detail`、
  `search_timeline`、`home_timeline` 两种模式、`following`；`fetch.is_following` 与
  `fetch.mutate` 不在 canary 覆盖内，匿名鉴权另有一条 canary。
- canary 红了会把原始响应写到端点目录的 `field_changed_<日期>.json`，内容未脱敏；
  redactor 只扫描 `raw/`，因此先人工检查、补全匹配提示并移到 `raw/`，再脱敏和 review。

---

## 7. 反模式（禁止）

- ❌ 手写 JSON 冒充真实响应（无法发现 X 改版）。
- ❌ 默认测试里访问真网络（不稳定、消耗配额、CI 会红得莫名其妙）。
- ❌ 用**错误文案字符串**做断言或逻辑判断。
- ❌ 只测 happy path。
- ❌ 为了"测试通过"去改断言，而不是查清 X 的真实行为。
- ❌ 在 fixture 里提交真实媒体 URL、cookie、他人隐私数据。

这些反模式在本仓库都留有现场案例：回放模式走签名加载会真的发网络请求（§9 第 7 条）；
断言写错会让你去修一个没坏的东西（§9 第 25 条）；fixture 混入真实数据靠脱敏脚本的自带断言兜底
（键序 / 假 id / 游标三类保真教训见 §9 第 12、13、17、18 条）；拿"你觉得不存在"的对象去测写操作
会真的改掉账号状态（§9 第 39 条）。

---

## 8. 质量门（Definition of Done）

一个功能只有同时满足以下条件才算完成：

1. 契约（`CONTRACT.md` + JSON Schema）已更新；
2. 有真实响应 fixture 的回放测试，覆盖正常 + 至少 2 类异常；
3. `cargo test` 离线全绿；`cargo clippy -- -D warnings` 干净；`cargo fmt --check` 通过；
4. 双形态契约测试一致；
5. 涉及下载的：本地 HTTP E2E 覆盖断点续传与完整性；
6. 将新增领域坑写入 `docs/02`；维护者也可同步记入本地操作手册，该手册不随仓库发布。

---

## §9 踩坑记录（2026-10-06 迁入自 AGENTS.md，保留原编号）

> 原文格式：**现象 → 根因 → 解法（含证据）**，正文与实测证据逐字保留；编号沿用 AGENTS.md
> 「踩坑记录」的全局编号，因此本节编号不连续。与前文各节有重叠的条目以「参见」互链。

### 6. **进程级单例（`OnceLock` 引擎）不能让并行测试共享。**

- 现象：双形态契约测试里，`auth.set_cookie` 的用例与"未注入凭据"的用例互相污染，
  库形态返回了网络上才会出现的错误——因为测试是多线程并行跑的，而引擎是进程级的。
- 解法：契约测试**合成一个 `#[test] fn`**，让顺序即用例表里那条隐式状态机。
  见 `bins/xspiderd/tests/contract_dual.rs` 顶部的注释。
- 参见：§4 契约测试——`contract_dual.rs` 顶部注释引用的正是该节的「不一致即缺陷」。

### 7. **回放（离线）模式不能走签名加载，否则会真的发网络请求。**

- 现象：离线跑 `fetch.get_user` 时报 `没有匹配的 fixture：GET tesla`——
  签名密钥要从 `x.com/tesla` 抓页面，回放里没有这条样本。
- 根因：签名是"真实会话"才有的东西；离线环境里没有会话。
- 解法：回放模式下**跳过签名**（记 debug 日志），签名的正确性交给 live recorder + canary
  覆盖（本次实测：真实请求返回 200，说明 `x-client-transaction-id` 被 X 接受）。见 ADR-010。
- 参见：§1 的离线默认与 §7 反模式「默认测试里访问真网络」——回放模式走签名是它最隐蔽的入口。

### 12. **`serde_json` 默认**不**保序，会让 fixture 失真。**

- 现象：把真实响应解析后再写回文件，键序被打乱，与真实响应无法直接 diff。
- 解法：workspace 级启用 `features = ["preserve_order"]`。见 ADR-017。
- 注意：它同时影响 `variables` 的键序（顺带与上游 `JSON.stringify` 对齐）。
- 参见：§2 fixture 保真——键序失真与内容失真同属"fixture 不再等于真实响应"。

### 13. **脱敏脚本必须自带断言，否则它"看起来跑完了"却把真数据写进了仓库。**

- 现象：给 redactor 加了一条 `if key.endswith("_id") { return value }` 的规则，
  它**提前 return 了原值**，于是 fixture 里 100+ 个 `rest_id` 全是真值——
  而脚本本身正常退出、文件正常写入，肉眼扫一遍根本看不出来。
- 根因：一条新规则插在了旧规则前面，短路了后面的替换；而当时没有"结果自检"。
- 解法：`script/redact_fixtures.py` 现在有四条事后断言——其中两条专门防这个：
  **原始长数字串 ∩ 输出 = ∅**（抓漏洗）与**不同原始 id 不得洗成同一假值**（抓冲突）。
  证据：加上断言后当场报出 `10 个原始数字串原封不动留在了输出里`。
- 教训：**脱敏这种"安全关键"的脚本，断言比逻辑更重要。**
- 参见：§2.1 脱敏规则与 §7 反模式「在 fixture 里提交真实媒体 URL、cookie、他人隐私数据」——redactor 的自检断言就是这条反模式的闸门。

### 17. **脱敏把不同 id 洗成同一个值，会让"端点逻辑"看起来坏掉。**

- 现象：`user_medias` 两页返回同一批 id；`user_tweets` 严格模式整页被去重干掉。
- 根因：第一版 `fake_numeric_id` 只按**长度**生成（"1" + 一堆 0），
  于是同长度的 id 全都变成同一个值 → 去重逻辑把整页删掉。
- 解法：假 id 按**值**的 crc32 生成（定长、纯数字、互不相同），
  并加"映射不得冲突"的断言。教训：**脱敏不能改变数据的结构性质**（唯一性就是其中之一）。
- 参见：§3「同一 ID 重复出现 → 去重后唯一」；§2.1「数字 ID 的假值须互不相同，避免改变去重结果」就是这条事故沉淀成的规则。

### 18. **`TEXT_KEYS` 里加一个 `"value"` 毁掉了所有游标。**

- 现象：分页测试报"游标没有推进"，两页的 `Bottom` 游标完全相同。
- 根因：游标条目的形状是 `{"cursorType": "Bottom", "value": "<游标>"}`——
  我把 `value` 当成"卡片文本"给脱敏成了固定假值，**两个页面于是有了同一个游标**。
- 解法：从 TEXT_KEYS 里去掉 `value`，卡片文本改用**按路径判定**
  （`path` 含 `binding_values` 才洗）。教训：**通用键名（value/name/text）要小心，
  它们在不同结构里的语义完全不同。**
- 参见：§3 的两行游标断言与 §2.1「保持分页字段 `cursor` 原样」。

### 25. **并发断言要断"语义"，不要断"实现细节上的巧合"。**

- 现象：并发上限测试报"峰值 4 > 上限 2"，而队列自己的日志显示同时只有 2 个任务在跑。
- 根因：我在本地 fixture server 里按 **socket 计数**，而 `/slow` 处理器在**最后一片写完之后还 sleep 了 50ms**，
  于是"服务端还以为在写"与"客户端已经读完、下一个任务开工"之间出现了重叠窗口 → 计数虚高。
- 解法：改成统计"**正在写字节**的请求数"，并把节流挪到每次写**之前**。
  教训：**断言写错比代码写错更贵**——它会让你去修一个没坏的东西。
- 参见：§5 表中「并发 5 任务 | 活跃数不超过配置上限」——现行判定依据是"正在流式传输的请求数"（`crates/xspider-download/tests/common/mod.rs`）。

### 39. **测试里"造一个不存在的对象"要挑结构上不可能的，不是"你觉得不存在"的。**

- 现象：为了验证写操作连线，我用推文 id `"1"` 当"不存在的推文"——**X 真的接受了那次点赞**，
  在用户的账号上留下了一个赞（发现后立刻 unfavorite 撤销了）。
- 根因：`"1"` 在语法上完全合法，而 X 的早期 id 空间里它确实对应一条推文。
- 解法：改用越界的长数字（X 会拒），并把断言放宽成"X 收到了并拒绝了"，
  而不是假设某种具体的拒绝理由（越界时 X 给的是 `ParseInt` 错误，**不是** 144）。
- 教训：**测写操作之前先问"这一下如果真的成功了，会在谁那里留下什么"**。
- 参见：§1 第 5 层 live 测试的 `XSPIDER_LIVE=1` 门控——写操作在真实账号上留下真实副作用，比只读 canary 更需要克制。
