# 06 · 真实消费方：接入手册与实测记录（M4）

> 这一册的产出方式：写一个**只经契约**的 CLI（`bins/xspider-cli`），拿它当"外壳的替身"
> 真跑一遍「取一页 → 下 3 个媒体 → 报告结果」（离线 + live 各一次），
> 然后如实记下——**接入需要动哪些代码、契约在哪里不够用**。
>
> 结论先放这里：**契约没有被推翻**。写这个 CLI 的过程中遇到的不一致中，
> 只有一处是组件侧的 bug（`since: 0` 被拒，已修），其余都是"文档该说清楚"或"外壳本来就要做的事"。

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
| `Services/FileIntegrity.swift`（大小/魔数/HTML 误页判定） | 下载队列（大小判据；**魔数判定尚未搬**，见 §5.4） |
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
5. **限流设置**：启动时 `net.set_limits` 一次（`cdn_concurrency` 目前只在队列创建时生效，见 §5.5）。
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

### 4.3 `dl.events` 的事件形状在 schema 里没有约束

schema 里写的是 `"events": { "items": { "type": "object" } }`——**等于没说**。
`kind`（`progress` / `completed` / `failed` / `skipped`）与各字段只在 `CONTRACT.md` 的散文里。
CLI 是照着散文写的，写对了，但机器可读的那份契约在这里是空的。

**修法（本轮已做）**：schema 增加 `$defs/downloadEvent`（`oneOf` 四种，各自
`additionalProperties: false`），并由契约守卫测试盯着"事件字段与 schema 一致"。
`CONTRACT.md` §4.13 也补上 `kind` 这个判别字段。

### 4.4 探测失败不能当致命错误（CLI 自己先写错过）

CLI 最初把 `net.probe_size` 的任何错误都当失败，于是**一次探测的传输抖动就废掉整轮**。
组件的语义是"探不到就按未知继续"（`Integrity::Unverified`、引擎选择按未知处理），
外壳照着做才对：大小是**可缺**的信息，真正的错误会在下载那一步以结构化的形态出现，
那里才有可操作的 `code`（`not_found` / `unauthorized` / …）。

### 4.5 已知的行为差异（不是 bug，接入时要接受）

| 差异 | 说明 |
|---|---|
| 引擎选择 | 外壳不再按 `estimatedSize`（码率×时长）判断，改由组件按 `requirements.segments` 与**探测到的真实大小**决定。那个估算实测差 5.25 倍（`docs/02` §E9），本来就只能当粗判。 |
| 置顶推文 | 组件会多返回一条（`TimelinePinEntry`，`docs/02` §H3）——参考实现没处理这条指令。UI 若不想显示置顶，自己按 `id` 过滤即可。 |
| 头像 | 组件给的是 `_bigger` 尺寸的归一化 URL，外壳那两行替换可以删。 |
| 完整性失败 | **已定：魔数/HTML 误页判定留在外壳**（`FileIntegrity` 不搬进组件）。组件负责"字节数与服务端声明一致"，外壳在收尾时再做一次"这真的是张图/这段真的是 mp4 吗"。理由见 §5.4。 |

---

## 5. 未决与建议

> **接口细节与调用时序以 [`07-API-REFERENCE.md`](07-API-REFERENCE.md) 为准；
> 组件与参考实现的能力对照以 [`08-CAPABILITY-MAP.md`](08-CAPABILITY-MAP.md) 为准。**
> 这一节只留**还没定的产品决定**。

1. **`net.probe_size` 与自动探测的默认值**（ADR-032/033）：组件默认在下载前探一次大小
   （每个未知大小的媒体多一次 CDN 请求），参考实现刻意不探。`XSPIDER_PROBE_SIZE=0` 可关。
2. ~~`system.version` 加 `transport` 字段~~ → **已做**（1.3.0）。
3. **双写记录**：参考实现的 `.downloaded.json` 与组件的 `downloads.json` 是两份。
   分工是清楚的（组件写它的、外壳写它的），但接入时要明确"谁负责在重启后对账"。
4. ~~魔数/HTML 误页判定搬进组件~~ → **已决定：不搬**（2026-10-01）。
   `FileIntegrity` 里的 `looksLikeTextError` / `hasKnownImageMagic` 留在外壳，理由是**分工**：
   组件回答"**字节对不对**"（这是它与服务端之间的事实，跨端一致、可离线测）；
   外壳回答"**这个文件对不对**"（这是产品语义——"图片"的定义、要不要为 HEIC 开例外、
   要不要把可疑文件挪进隔离目录，各家外壳可以不同）。
   两者都不省：组件那条挡住截断，外壳那条挡住"CDN 用 HTML 错误页凑够字节数"。
   参考实现原本就是两条都做（`FileIntegrity.verify` 在 `finalizeDownload` 里），
   接入后这个位置不变，只是它前面的字节数校验改由组件负责。
5. **`net.set_limits` 的并发上限**只在队列创建时生效（信号量不能缩容）。若外壳要"运行中调并发"，需要再改。

### 5.1 审计出来的两处，都在**外壳**侧（组件不用改）

- **暂停/恢复与失败重试没有接线**：外壳的"继续"与"重试"走的是"重新 `dl.enqueue`
  （同一个 `job_id`）"，而组件对已存在的 `job_id` 返回 `already_known` 且**不会重启任务**
  （幂等是刻意的）。症状是界面显示"下载中"、进度永远不动。
  修法：改调 **`dl.resume`**（对 `paused` 与 `error` 都有效，只拒绝 `complete`）。
- **重启后不与组件对账**：启动时把在飞任务一律标成 `paused`，而事件轮询只在"有新任务入队"
  时才启动，于是"只有恢复任务、没有新任务"的那次启动不会去问 `dl.list()`。
  修法：启动时无条件跑一次对账。

（完整的能力对照与其余缺口见 [`08-CAPABILITY-MAP.md`](08-CAPABILITY-MAP.md) §5。）

---

## 6. 这一册什么时候该更新

每接一个新消费方（或者同一个消费方改形态：sidecar ↔ cdylib），回来更新 §3 的清单与 §4 的发现。
`docs/01` 讲的是**组件自己的边界**，这一册讲的是**别人接它时会遇到什么**——两者不要混。

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

**这一轮踩到的两个 Swift 6 细节**（下次接入别的外壳也会撞上）：

- `Any` 不是 `Sendable`：`[String: Any]` 跨 actor 边界会被拒（
  `sending 'params' risks causing data races`）。解法是给契约参数/结果一个
  `Sendable` 的 `JSONValue` 枚举（`Services/XSpiderJSON.swift`）。
- `NSLock.lock()/unlock()` 在 async 上下文里被标记为不可用，要用作用域式的
  `withLock`；`ISO8601DateFormatter` 这类格式化器要显式 `nonisolated(unsafe)` +
  只配置一次（每次新建才是真的坑）。
