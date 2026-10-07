# 06 · 真实消费方：接入手册与实测记录（M4）

> 这一册的产出方式：写一个**只经契约**的 CLI（`bins/xspider-cli`），拿它当"外壳的替身"
> 真跑一遍「取一页 → 下 3 个媒体 → 报告结果」（离线 + live 各一次），
> 然后如实记下——**接入需要动哪些代码、契约在哪里不够用**。
>
> 结论先放这里：**契约没有被推翻**。写这个 CLI 的过程中遇到的不一致中，
> 只有一处是组件侧的 bug（`since: 0` 被拒，已修），其余都是"文档该说清楚"或"外壳本来就要做的事"。
>
> 本册只写**跨消费端通用**的内容；`x-spider-mac`（第一个真实外壳）的专项接入记录
> 已迁 [`docs/history/mac-integration-2026-10/`](history/mac-integration-2026-10/MIGRATION.md)。

---

## 1. CLI 是什么，为什么它值得存在

```bash
# 离线：不碰网络，用真实响应 fixture 回放，CI 里可跑
./target/debug/xspider-cli --screen-name demo_user --fixture-dir fixtures --dry-run --json

# live：真的取一页、真的下 3 个媒体（需要 XSPIDER_COOKIE 与代理）
./target/debug/xspider-cli --screen-name tesla --count 3 --out ./downloads \
  --proxy "$XSPIDER_PROXY" --segments 4
```

它的**唯一特殊性**是：它不 `use` 任何 `xspider-*` crate（守卫测试
`bins/xspider-cli/tests/only_the_contract.rs` 会读它自己的 `Cargo.toml` 来强制这一条）。
也就是说它和"用别的语言写的外壳"处境相同——**契约缺什么、哪里别扭，它第一个炸**。
组件自己的测试做不到这一点：它们总能顺手 `use xspider_fetch::Post`。

它还顺带示范了三件外壳必须自己做的事（写进代码里，不是写在文档里）：
握手与版本纪律、目录与文件名的计算、进度节流。

### 1.1 通用接入主题：本册留结论，细节在 `docs/07`

| 主题 | 结论 | 详见 |
|---|---|---|
| 传输形态取舍（HTTP / stdio / cdylib） | **sidecar + HTTP 是主形态**（换组件 = 换一个二进制）；stdio 给"不想要端口"的场景（容器、管道）：stdout 从头到尾只走 JSON Lines、ready 行写 stderr、不使用 token；cdylib 只在需要进程内调用且外壳**没有**开 hardened runtime 时用（dlopen 会被 library validation 拒）。运行中用 `system.version` 的 `transport` 字段判形态，不要硬编码"我是 sidecar" | [`07` §1.3、§2.1a、§3](07-API-REFERENCE.md)；契约侧见 [`CONTRACT.md` §2](CONTRACT.md) |
| state dir | `--state-dir` / `XSPIDER_STATE_DIR` 决定 version 1 下载记录的路径，并**同时取得 sidecar 单实例锁**（Unix 用内核 `flock`）；两者同时设置时 flag 优先；**多实例必须各不相同**。启用后重启按状态恢复：`waiting`/`active` 恢复为 `waiting`，`paused` 保持，`error`（含取消）不自动重试 | [`07` §2.1（六条）、§2.2、§2.4](07-API-REFERENCE.md) |
| 事件轮询 | **没有推送式事件流**（ADR-029）：`dl.events {since}`（**第一次传 0**）带游标取增量画进度；"到底结束了没有"用 `dl.list`（权威）。**节流是外壳的事**：进度事件按片发，外壳自己决定多久刷新一次 | [`07` §4.4、§7.2](07-API-REFERENCE.md) |
| 升级兼容 | 启动时 `system.version` 对**主版本**握手：不匹配就拒绝启动并给可操作提示，不要降级成"部分功能可用"；再用 `system.methods` 自检"我依赖的 method 是否都在"。契约对 method / 字段 / 错误码**只增不改不删**，破坏性变更必须写 ADR | [`07` §1.4](07-API-REFERENCE.md)；兼容规则见 [`CONTRACT.md` §6](CONTRACT.md) |

---

## 2. 真实跑出来的证据（2026-10-01）

**离线（CI 可复现）**：

```
用户：demo_user（Demo User）id=50909294 媒体数=56
取到一页：3 条推文 / 3 个媒体 · 服务端还有更多（游标可续） · 计划 3 个
大小：3 个都未知（离线回放不联网，或服务端没给 Content-Length）
  video  未知  …-1.mp4  → <out>/2009-09-30 12-34-56 demo_user …-1.mp4
--dry-run：只规划，不入队、不下载
```

**live（`--segments 4`，走 Aria2Next）**：

```
用户：Tesla（Tesla）id=13298072 媒体数=2041
取到一页：13 条推文 / 3 个媒体 · 服务端还有更多（游标可续） · 计划 3 个
大小合计 84.3 MiB（3 个已知 / 0 个未知）
  video   3.0 MiB  …-1.mp4
  video  68.6 MiB  …-1.mp4
  video  12.8 MiB  …-1.mp4
下载：3/3 成功 · 0 失败 · 落盘 84.3 MiB · 用时 40.4s
```

每个事件里的完整性结论都是 `已校验 <actual>/<expected>`，且**落盘字节数与探测值逐字节相同**
（3122633 / 71884943 / 13374277）。退出码 0。

---

## 3. 历史消费端接入记录

> **已迁至 [`docs/history/mac-integration-2026-10/MIGRATION.md`](history/mac-integration-2026-10/MIGRATION.md)（历史记录）。**

---

## 4. 契约与实现的不一致（消费者视角抓到的）

> 这一节是 M4 最值钱的部分：**只有真用的人才会撞上**。

### 4.1 `dl.events` 的 `since: 0` 被拒（**已修**）

契约写的是"`since`：上一次拿到的 `seq`；**从 0 开始**"，而组件回
`invalid_request: since 必须是正整数`——校验把 0 一刀切掉了。
CLI 于是**在下载已经跑起来之后**才失败（前 3 个任务已入队），这类"跑到一半才炸"的最难查。

修法：把"可选整数"拆成两个——
`optional_u64`（允许 0，用于 `since` 与 `expect_size`，后者的 schema 里 `minimum` 就是 0）
与 `optional_positive_u64`（拒绝 0，用于 `count`）。
并补了测试 `zero_is_legal_where_the_contract_says_so`。
**教训：契约与实现不一致时，错的通常是实现**——文档是被人当作承诺读的那一份。

### 4.2 `system.shutdown` 不在 `system.methods` 里

按 `docs/CONTRACT.md` §3.2 它是 **sidecar 传输层 method**（cdylib 没有"关掉宿主进程"这回事），
所以不出现在能力清单里。这是**有意的**，但代价是：只读 schema 的外壳不知道它存在。

**提案（已做，1.3.0）**：`system.version` 的结果里加了 `transport`
（`"sidecar" | "cdylib"`）。这样外壳不必"因为我是我"而硬编码能力差异，而是问一句
"我是不是 sidecar"；`system.shutdown` 仍然只属于传输层、不进能力清单。

### 4.3 `dl.events` 的事件形状在 schema 里没有约束（历史问题，现已修）

schema 里写的是 `"events": { "items": { "type": "object" } }`——**等于没说**。
`kind`（`progress` / `completed` / `failed` / `skipped`）与各字段只在 `CONTRACT.md` 的散文里。
CLI 是照着散文写的，写对了，但机器可读的那份契约在这里是空的。

**修法（本轮已做）**：schema 增加 `$defs/downloadEvent`（`oneOf` 四种，各自
`additionalProperties: false`），并由契约守卫测试盯着"事件字段与 schema 一致"。
`CONTRACT.md` §4.13 也补上 `kind` 这个判别字段。`crawl.run` 事件另有明确的
`crawlStats` / `crawlEvent` / `stampedCrawlEvent` schema 定义，见 `CONTRACT.md` §4.14。

### 4.4 探测失败不能当致命错误（CLI 自己先写错过）

CLI 最初把 `net.probe_size` 的任何错误都当失败，于是**一次探测的传输抖动就废掉整轮**。
组件的语义是"探不到就按未知继续"（`Integrity::Unverified`、引擎选择按未知处理），
外壳照着做才对：大小是**可缺**的信息，真正的错误会在下载那一步以结构化的形态出现，
那里才有可操作的 `code`（`not_found` / `unauthorized` / …）。

### 4.5 已知的行为差异（不是 bug，接入时要接受）

| 差异 | 说明 |
|---|---|
| 引擎选择 | 外壳不再按 `estimatedSize`（码率×时长）判断，改由组件按 `requirements.segments` 与**探测到的真实大小**决定。那个估算实测差 5.25 倍（`docs/02` §E9），本来就只能当粗判。 |
| 置顶推文 | 组件会返回 `TimelinePinEntry`（`docs/02` §H3）。消费端若产品上不想单独展示置顶项，应在应用层按自己的展示规则处理；不要改 raw 解析去“吃掉”它。 |
| 头像 | 组件给的是 `_bigger` 尺寸的归一化 URL，外壳那两行替换可以删。 |
| 完整性失败 | **已定：魔数/HTML 误页判定留在外壳**（`FileIntegrity` 不搬进组件）。组件负责"字节数与服务端声明一致"，外壳在收尾时再做一次"这真的是张图/这段真的是 mp4 吗"。理由见 §5 第 4 条。 |

---

## 5. 已决事项与仍待处理

> **接口细节与调用时序以 [`07-API-REFERENCE.md`](07-API-REFERENCE.md) 为准；
> 组件当前能力与实现位置以 [`08-CAPABILITY-MAP.md`](08-CAPABILITY-MAP.md) 为准**
> （迁移期与 `x-spider-mac` 的逐项 parity 对照已归档：
> [`docs/history/mac-integration-2026-10/CAPABILITY-PARITY.md`](history/mac-integration-2026-10/CAPABILITY-PARITY.md)）。
> 下列记录区分已决事项、已接线事项与仍待处理的接口约束。

1. **`net.probe_size` 与自动探测默认值**（ADR-032/033）：**已决定**组件默认在下载前探测
   未知大小（每个未知大小的媒体多一次 CDN 请求）；`XSPIDER_PROBE_SIZE=0` 可关。
2. ~~`system.version` 加 `transport` 字段~~ → **已做**（1.3.0）。
3. **双写记录与重启对账**：分工已明确，组件持有自己的 version 1 持久记录，外壳保留产品历史；
   外壳启动时以 `dl.list()` 对账。队列恢复规则见 `07` §2.2；Waiting/Active 恢复为 waiting，
   Paused 保持暂停，Error（含取消）不会自动重试。
4. ~~魔数/HTML 误页判定搬进组件~~ → **已决定：不搬**（2026-10-01）。
   `FileIntegrity` 里的 `looksLikeTextError` / `hasKnownImageMagic` 留在外壳，理由是**分工**：
   组件回答"**字节对不对**"（这是它与服务端之间的事实，跨端一致、可离线测）；
   外壳回答"**这个文件对不对**"（这是产品语义——"图片"的定义、要不要为 HEIC 开例外、
   要不要把可疑文件挪进隔离目录，各家外壳可以不同）。
   两者都不省：组件那条挡住截断，外壳那条挡住"CDN 用 HTML 错误页凑够字节数"。
   当前分工固定为“两层都做”：组件负责字节一致性，消费端按自己的产品语义做文件类型 / 内容校验。
5. **`net.set_limits` 的并发上限**只在队列创建时生效（信号量不能缩容）。若外壳要"运行中调并发"，需要再改；这是本节唯一仍待决定的产品行为。

### 5.1 审计出来的两处，都在**外壳**侧 —— **均已修**

> **已迁至 [`docs/history/mac-integration-2026-10/MIGRATION.md`](history/mac-integration-2026-10/MIGRATION.md)（历史记录）。**
> 相关的通用语义（`already_known` 是幂等命中、恢复要用 `dl.resume`、启动对账）见 [`07` §4.4 与 §7.3](07-API-REFERENCE.md)。

---

## 6. 这一册什么时候该更新

每接一个新消费方（或者同一个消费方改形态：sidecar ↔ cdylib），回来更新 §4 的发现
与 §5 的状态。某个消费端专属的改动清单与接入实录，写进
`docs/history/<消费端>-<日期>/`（目录约定见 [`history/README.md`](history/README.md)），
不留在本册。
`docs/01` 讲的是**组件自己的边界**，这一册讲的是**别人接它时会遇到什么**——两者不要混。

---

## 7. 历史接入实录

> **已迁至 [`docs/history/mac-integration-2026-10/MIGRATION.md`](history/mac-integration-2026-10/MIGRATION.md)（历史记录）。**
