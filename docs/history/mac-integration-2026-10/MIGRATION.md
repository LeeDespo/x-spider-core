# MIGRATION —— `x-spider-mac` 接入迁移实录（2026-10-01）

> 本文是历史记录：`x-spider-mac`（第一个真实外壳）在 2026-10-01 接入 `x-spider-core` 时的
> 改动清单、审计修法与实测证据。当时的回退点是 `x-spider-mac` 仓库 tag
> `pre-component-integration`。
>
> 来源：由 `docs/06-CONSUMER-INTEGRATION.md`（2026-10-05 版）的 §3、§5.1 与 §7 原文迁入，
> 章节编号沿用原文；文内指向 `08-CAPABILITY-MAP.md` 的对照链接改指同目录的
> [`CAPABILITY-PARITY.md`](CAPABILITY-PARITY.md)。当前接入规则见
> [`docs/06-CONSUMER-INTEGRATION.md`](../../06-CONSUMER-INTEGRATION.md)。

---

## 3. 接入 `x-spider-mac`：改动清单

按 ADR-005，接入初期**由外壳提供 aria2 路径**（复用现有分发）；按 ADR-002，
形态从 sidecar 起步（cdylib 只在需要进程内调用时才用，行为一致由
`bins/xspiderd/tests/contract_dual.rs` 保证）。

### 3.1 可以直接删掉 / 停用的（组件已经覆盖）

| 外壳里的东西 | 现在归组件 |
|---|---|
| `Services/XClientTransaction.swift`（签名） | 内核的 `x-clid` 签名 + 失效重取 |
| `Services/SearchQueryIdProvider.swift`（queryId 自愈） | 取数组件（锚定 `operationName`，404 才自愈） |
| `Services/RequestGate.swift` + `NetworkClient` 的重试/熔断 | 内核的限流闸门与 429 熔断（按配额域分桶） |
| `TwitterAPI.swift` 里的 `mapTwitterPost` / `mapTwitterMedias` / `extract*` | 取数组件的 DTO 映射（含两种用户结构、置顶条目、长推文正文、头像归一化） |
| `Services/Aria2Engine.swift` + `Aria2RPCClient.swift` | 下载组件的外派后端 |
| `Stores/DownloadStore.swift` 的调度/重试/完整性/临时文件 | 下载队列（`job_id` 幂等、epoch、完整性校验、落盘字节数判据） |
| `Services/FileIntegrity.swift`（大小/魔数/HTML 误页判定） | 下载队列（大小判据；**魔数判定尚未搬**，见 §5 第 4 条） |
| TS 侧 `src/twitter/api.ts` / `src/utils/aria2.ts` / `src/stores/download.ts` | 同上 |

### 3.2 必须保留下来的（组件不碰，这是分工）

- **文件名模板**（`Services/FileNameTemplate.swift`，13 个变量）：契约里 `file_name` 由外壳算好。
  CLI 里照抄了默认模板的 5 个变量作为示范（`bins/xspider-cli/src/plan.rs`），
  连"时间缺失时用 `未知日期`""Windows 保留名加 `!`"都照抄了——接入时这段**原样不动**。
- **目录选择**（`AppDirectories` / `accountSubfolderEnabled` / 设置项）。
- **`.downloaded.json` 同文件跳过**：可以继续用（零改动），也可以改用 `job_id` 幂等 +
  `dl.list()` 对账。后者更省事，但那是产品决定，不是接入必须。
- **UI、通知、进度条**：组件只发事件，不做通知。
- **UI 的健康状态展示**（"被限流了 / 未登录"）：判定来源改成 `net.status` 与错误的 `code`。

### 3.3 需要新写的（外壳侧，估算 ~1–2 天）

1. **sidecar 生命周期**：起进程 → 读 stdout 的 `ready` 行拿 `port`/`token`/`version` →
   用 `X-XSpider-Token` 调 `POST /` → 退出时先 `system.shutdown` 再兜底 kill。
   **多实例必须各自独立端口与 state-dir**（`docs/03` §3），否则会和别的消费方互踩。
2. **契约版本握手**：主版本不同就拒绝启动并给可操作提示（`docs/CONTRACT.md` §6）。CLI 里
   `WRITTEN_AGAINST` 那段就是示范。
3. **凭据注入**：Keychain → `auth.set_cookie`。**只进不出**：不落盘、不打日志、不进 UI 日志。
4. **代理**：每次变化都调 `net.set_proxy`（本机实测端口一天变好几次）。
   实测：改了代理之后取数与下载**都会跟着换**（ADR-035），不需要重建队列。
5. **限流设置**：启动时 `net.set_limits` 一次（`cdn_concurrency` 目前只在队列创建时生效，见 §5 第 5 条）。
6. **aria2 路径**：`XSPIDER_ARIA2_PATH` 或配置里指定（ADR-005 的 B 方案）。
   不配也能跑（自动降级到内置后端），但那样就少了多连接。
7. **对 `transport` 做有界重试**：组件内部每次都重试 3 次（150ms/450ms 退避），
   但实测本机代理会出现**持续一两秒的整段拒连**，那点预算不够（本次 live 跑里撞到 2 次）。
   CLI 的做法是 1s / 3s 两跳，且**只重试 `transport`**——契约错误一律直接返回。
8. **事件轮询驱动 UI**：`dl.events {since}` 取增量画进度，`dl.list` 决定"结束了没有"。
   **节流是外壳的事**（契约原话）：组件按片发事件，CLI 用"每 5 个百分点一行"。
9. **时区**：组件给的是 **UTC**（`docs/02` §D3，组件不做时区换算），
   而文件名模板要的是**本地时间**——转换留在外壳。

---

## 5.1 审计出来的两处，都在**外壳**侧 —— **均已修**

- **暂停/恢复与失败重试没有接线**：外壳的"继续"与"重试"走的是"重新 `dl.enqueue`
  （同一个 `job_id`）"，而组件对已存在的 `job_id` 返回 `already_known` 且**不会重启任务**
  （幂等是刻意的）。症状是界面显示"下载中"、进度永远不动。
  修法：改调 **`dl.resume`**（对 `paused` 与 `error` 都有效，只拒绝 `complete`）。
- **重启后不与组件对账**：启动时把在飞任务一律标成 `paused`，而事件轮询只在"有新任务入队"
  时才启动，于是"只有恢复任务、没有新任务"的那次启动不会去问 `dl.list()`。
  修法：启动时无条件跑一次对账。

**修法（已做）**：前者在外壳把任务交回组件的唯一入口按 `dl.enqueue` 返回的 `accepted_by`
分支——命中 `already_known` 就补一次 `dl.resume`；后者在启动时无条件对账一次，
并把组件自动重新排队的任务按本应用的策略（重启不自动续传）真的暂停掉。
另外本轮还修掉了**切换账号**：`getAccountInfo(cookieStringOverride:)` 以前忽略了那个参数，
于是"验证新 cookie"实际验证的是**旧会话**，再把"旧账号名 + 新 cookie"存成一对——
列表里两条账号记录因此存着逐字节相同的 cookie，切谁都是同一个账号。

另外，"创建任务"那条下载线已经改走组件的 `crawl.run`（翻页与四条终止判据不再由外壳实现）——
接线时逐条处理的代价（分块、挂起、日期边界、媒体类型、候选有损）见
[`CAPABILITY-PARITY.md`](CAPABILITY-PARITY.md) §5.4。

（完整的能力对照与其余缺口见 [`CAPABILITY-PARITY.md`](CAPABILITY-PARITY.md) §5。）

---

## 7. 接入实录（2026-10-01，`x-spider-mac`）

**已经接上的**（分支 `feat/xspider-core-integration`，回退点 tag `pre-component-integration`）：

| 项 | 状态 |
|---|---|
| 组件运行时（起进程 / ready 行握手 / JSON-RPC / 崩溃自愈 / 退出时优雅关停） | ✅ `Services/XSpiderComponent.swift` |
| JSON → 应用模型（`TwitterPost` / `TwitterUser` / `TwitterMedia` / `ReplyNode` 含深度） | ✅ `Services/XSpiderMapping.swift` |
| 取数全部走组件（getUser / getFollowing / getUserMedias / getUserTweets / getHomeTimeline / searchTimeline / getTweet / getTweetDetailTree） | ✅ 签名不变，调用方零改动 |
| 删除已被替代的实现 | ✅ `SearchQueryIdProvider.swift`（queryId 自愈）、`Aria2Engine.swift` + `Aria2RPCClient.swift`（下载引擎，−827 行） |
| **下载也交给组件**（`dl.enqueue` + `dl.events`/`dl.list` 轮询） | ✅ `DownloadStore` 只算目录与文件名、收尾时做内容校验、写下载记录 |
| 组件的**外部目录**部署（换文件即换组件） | ✅ `~/Library/Application Support/moe.keli.xspider.mac/XSpiderCore/` |

实测证据（`Tests/XSpiderMacTests/ComponentLiveTests.swift`，`TEST_RUNNER_XSPIDER_LIVE=1` 时跑）：

```
Test Case 'testComponentIsReachableAndReportsTransport' passed (0.002 seconds)
Test Case 'testFetchUserThroughTheAppPath'              passed (2.441 seconds)
Test Case 'testFetchUserMediasThroughTheAppPath'        passed (2.981 seconds)
Test Case 'testDownloadThroughTheStoreAndComponent'     passed (5.376 seconds)   ← 下载迁移的验收
```

应用侧离线测试：**274 条 0 失败**（`xcodebuild test`，live 那 4 条默认跳过）。

另外实测：应用退出后组件进程**一并消失**（无残留）——那正是 `applicationShouldTerminate`
返回 `.terminateLater` 换来的。

**已经全部接完**（2026-10-01 晚）：`auth.whoami` / `fetch.is_following` / `fetch.mutate`
补进组件之后，应用里**再没有一条自己发出的 X 请求**——
`NetworkClient` / `XClientTransaction` / `RequestGate` 及其测试已删除（−1042 行）。
`TwitterAPI` 现在只是"契约 JSON ↔ 应用模型"的映射层。

实测：离线 235 条 0 失败；live 8 条全过；应用启动后组件是它的子进程（两条 ESTABLISHED
连接）、退出时组件随之消失、无残留。

**还没接的**（剩下的都是"产品决定"而非能力缺口）：
3. 取数侧的旧解析函数（`extractPostsFrom*` / `mapTwitterPost` / `extractReplyNodes` 等）
   现在只有测试在引用——随测试一起删，属于"清尾"。

**这一步踩到的第三个坑（值得单独记）**：迁移后一条既有的计时测试开始失败
（基线 3.9s 通过 → 迁移后 30.5s）。根因不是新代码慢，而是我把 `configure` 里的
`client.invalidate()` 一并删了——**还有两条路在用本机的 `NetworkClient`**
（关注态查询、账户探测），旧连接池没人关，于是一条失败的后台重试一直占着**请求闸门**，
把同一闸门下的其它请求拖到超时。
教训：**"这个模块已经没人用了"要按引用数核对，不能按感觉**；删一层实现时，
先 grep 谁还在用它的资源管理代码。

**这一轮踩到的三个 Swift 6 细节**（下次接入别的外壳也会撞上）：

- `Any` 不是 `Sendable`：`[String: Any]` 跨 actor 边界会被拒（
  `sending 'params' risks causing data races`）。解法是给契约参数/结果一个
  `Sendable` 的 `JSONValue` 枚举（`Services/XSpiderJSON.swift`）。
- `NSLock.lock()/unlock()` 在 async 上下文里被标记为不可用，要用作用域式的
  `withLock`；`ISO8601DateFormatter` 这类格式化器要显式 `nonisolated(unsafe)` +
  只配置一次（每次新建才是真的坑）。
- **独占性冲突会让应用直接 SIGABRT**，而且极难从现象反推。建回复树时写成
  `byId[id]?.depth = depth(of: id)`：左侧持有对 `byId` 的写访问，右侧的嵌套函数
  里又要读同一个字典——嵌套函数捕获的本地 var 走同一个访问盒，运行时检查直接
  `fatalError`。症状是**"打开任何带评论的推文，应用必崩"**。
  解法：把右侧先算进局部变量（`let computed = depth(of: id)`），写访问不再跨越那次读取。
  两个教训：① 嵌套函数/闭包一旦捕获本地 `var`，**同一条语句里"写它 + 读它"就是雷**；
  ② 迁移之后**旧测试测的是旧代码**——回复树的旧用例测的是已经没人调用的解析函数，
  新映射一路亮绿灯，直到用户点开一条带评论的推文。
  回归用例：`XSpiderMacTests/XSpiderMappingTests.swift`。

---

## 踩坑记录（2026-10-06 迁入自 AGENTS.md，保留原编号）

> 以下 1 条原载于根目录 `AGENTS.md`「踩坑记录」，属 mac 接入专项，按原编号与
> 原加粗标题句全文收录于此；上文 §7「三个 Swift 6 细节」的第三条是它的精要版。
> 其余主题的条目分入 docs/02 / 03 / 04 / 05 / 12，总索引见 `docs/05` §8。

**42. `byId[id]?.depth = depth(of: id)` 这种"同一句里既写又读同一个本地 var"会让应用 SIGABRT。**
- 现象：接入后**打开任何带评论的推文，应用必崩**（`EXC_CRASH / SIGABRT`）。
  崩溃栈：`MediaDetailView.loadReplies` → `getTweetDetailTree` →
  `XSpiderMapping.replyNodes` → `_swift_reportExclusivityConflict` → `abort`。
- 根因：`byId` 是本地 `var`，被嵌套函数 `depth(of:)` 捕获后，所有访问都走同一个**访问盒**。
  左侧 `byId[id]?.depth =` 持有对它的**写访问**，而右侧调用 `depth(...)` 时又要读它——
  Swift 运行时的独占性检查（`swift_beginAccess`）直接 `fatalError`。
- 解法：把右侧先算进局部变量（`let computed = depth(of: id)` / `byId[id]?.depth = computed`），
  让写访问不再跨越那次读取。
- 教训一：**嵌套函数/闭包一旦捕获本地 `var`，"同一条语句里写它 + 读它"就是雷**——
  错误信息只会说 `Simultaneous accesses`，从"点开评论就崩"根本反推不到这里。
- 教训二：**迁移之后，旧测试测的是旧代码。** 回复树的旧用例测的是已经没人调用的
  旧解析函数，新的映射文件一路亮绿灯（239 条全过），直到用户点开一条带评论的推文。
  搬一层实现时，**要按"新代码有没有测试"重新数一遍覆盖**，不能看总条数。
- 回归用例：`x-spider-mac` 的 `XSpiderMacTests/XSpiderMappingTests.swift`。
  已验证它真的抓得住：把错误写法放回去 → 测试宿主崩溃、4 条全红。
