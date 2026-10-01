# 08 · 能力对照：组件 ↔ `x-spider-mac`

> 这一册回答一个问题：**参考实现（`x-spider-mac`）能做的事，组件是不是都覆盖了？**
>
> 对照基准是 `x-spider-mac` 的 tag **`pre-component-integration`**——即"接入组件之前"的那个状态。
> 它列的是**能力**（能取什么、能下什么、什么情况下停），不是代码结构：
> 组件有自己的分层，不要求与参考实现长得一样（`AGENTS.md`「参考实现纪律」第 4 条）。
>
> 三张表之后是分工说明与缺口清单。**缺口分两类**：组件侧的（要改契约）与外壳侧的（要接线）。

---

## 1. 取数

| 参考实现里的函数（`pre-component-integration`） | 组件 method | 说明 |
|---|---|---|
| `getUser(screenName:fast:)` | `fetch.get_user` | 结构对齐（`legacy` / `core` 两种形态都认） |
| `getUserMedias(userId:cursor:count:fast:)` | `fetch.user_medias` | 广告过滤、去重、置顶推文（`TimelinePinEntry`） |
| `getUserTweets(userId:cursor:count:requireMedia:includeRetweets:)` | `fetch.user_tweets` | 两个开关语义一致（转推展开、`retweeted_by`） |
| `getTweet(id:)` | `fetch.tweet_detail` | 组件同时给出 `focal` 与 `replies[]` |
| `getTweetDetailTree(id:)` | `fetch.tweet_detail` | 组件给**扁平**回复列表（含 `parent_id` / `is_partial_parent`），建树留在外壳 |
| `searchTimeline(screenName:range:product:count:cursor:)` | `fetch.search_timeline` | 日期语义一致（`until` 含当天、组件内部 +1 天、不做时区换算） |
| `getHomeTimeline(mode:cursor:)` | `fetch.home_timeline` | 推荐 / 关注两种模式 |
| `getFollowing(userId:cursor:count:)` | `fetch.following` | 默认每页 100 |
| `getAccountInfo(...)` / `probeConnection()`（抓首页 HTML + 正则） | `auth.whoami` | **登录校验语义完全一致**：页面里没有 `screen_name` 就是 cookie 失效 |
| `isFollowing(screenName:useCache:)`（v1.1 + 300 秒内存缓存） | `fetch.is_following` | 缓存的是"我是谁"（不是被查的人），换 cookie 自动失效 |
| `currentUserId()` | 无单独 method | 外壳从 `auth.whoami` 的 `account.id` 取；缺失时用 `fetch.get_user` 补 |

**解析层面的差异（都是有意的，且都是改进）**：

| 参考实现 | 组件 |
|---|---|
| 长推文正文取 `legacy.full_text`（截断，以 `…` 结尾） | 优先 `note_tweet.note_text`，并按参考实现清洗媒体占位链接与短链 |
| 头像替换 `_normal` → `_bigger` 散在各处 | 组件统一归一化（`https:` + `_bigger`），外壳那两行可以删 |
| 置顶推文（`TimelinePinEntry`）没处理 | 组件会返回置顶推文（它是独立指令，可能整页只有它） |
| 搜索 queryId 用内置常量 + 自愈 | 同样自愈，但**锚定 `operationName`**（避免抓到别的操作的 id） |
| 引用推文内嵌正文（`quoted_status_result`） | **只给 `quoted_id`** —— 见 §5.1 的缺口 |

---

## 2. 写操作与账户

| 参考实现 | 组件 method | 说明 |
|---|---|---|
| `favoriteTweet(id:)` | `fetch.mutate { action: "favorite", tweet_id }` | |
| `unfavoriteTweet(id:)` | `… "unfavorite"` | |
| `createRetweet(id:)` | `… "retweet"` | |
| `deleteRetweet(id:)` | `… "unretweet"` | |
| `createBookmark(id:)` | `… "bookmark"` | |
| `deleteBookmark(id:)` | `… "unbookmark"` | |
| `followUser(screenName:)` | `… "follow", screen_name` | 成功后失效关注态缓存 |
| `unfollowUser(screenName:)` | `… "unfollow", screen_name` | 同上 |

**一处行为对齐（很重要）**：X 对失败的突变返回的是 **HTTP 200 + `errors[]`**，
只看状态码会给出"已点赞"的假象。参考实现在 `mutate` 之后检查了响应体；
组件同样检查（`ensure_mutation_succeeded`），并把上游错误码翻译成契约的 `error.code`。

---

## 3. 下载

| 参考实现 | 组件 | 说明 |
|---|---|---|
| `Aria2RPCClient`（起 aria2Next、JSON-RPC、`tellStatus` 轮询） | 下载组件的 **Aria2Next 后端** | 启动时校验 `product=aria2-next`；引擎选择留在组件内部 |
| `Aria2Engine`（RPC 优先、子进程兜底、resume、多连接、UA/Referer、代理） | 同上 | UA / Referer / 代理**逐任务显式传**（子进程不认识 `HTTPS_PROXY`） |
| `DownloadStore` 的 URLSession 下载 | **内置 HTTP 后端** | 流式落盘、Range 续传、断流重试、原子 rename、取消清理 |
| 引擎分流 `engineFor`（按"码率×时长"估算挑引擎） | 组件内部按 `requirements.segments` + **探测到的真实大小** | 那个估算实测差 **5.25 倍**，已被"问 CDN"取代 |
| `pump()` 并发调度 + CDN 限流降级（并发降到 1） | 下载队列的并发上限 + `cdn_concurrency` | 接口与 CDN 是两套配额 |
| 重试 5 次、指数退避 1/2/4/8/16 秒 | 组件内部对单次请求重试；**外壳侧的重试仍在** | 见 §5.2 |
| `finalizeDownload`：原子移动到目标 + 校验 + 写记录 | 组件：**落盘字节数**判据 + 完整性校验 + 记录（带 `version`） | 完成判据不是"后端说成功"（Aria2Next 对 404 会报 complete 并留 0 字节文件） |
| 暂停（`resumeDataMap` / aria2 pause）、取消（删除文件与控制文件） | `dl.pause` / `dl.resume` / `dl.cancel` | 暂停**保留断点**，取消**丢弃断点**；断点名带引擎标识，两个引擎的断点物理上不能混用 |
| 记录文件 `.downloaded.json`（每个用户目录一份） | 组件写自己的 `downloads.json`（`XSPIDER_STATE_DIR`） | 两份记录分工不同：组件那份用于**重启续传**，外壳那份用于**同文件跳过** |
| 临时文件 `<gid>-<name>` / `.xspider-tmp-*` | 组件：`.part.<engine>` 命名 | 换引擎不会误用另一个引擎的断点 |
| `FileIntegrity`（大小 + 魔数 + HTML 误页） | 组件只管**大小**；魔数判定**有意留在外壳** | 分工见 §4 |
| 下载请求头（UA Chrome 142、`Referer: https://x.com/`） | 与参考实现一致 | 另加 `Accept-Encoding: identity`（否则压缩会让字节数对不上，完整性**假红**） |

---

## 4. 爬取与调度

| 参考实现 | 组件 |
|---|---|
| `SyncStore`：每用户最多 5 页、按 `.synced.json` 的 `anchorDay` 增量 | `crawl.run` 的 `limits.max_pages` + `strategy.since/until`（**外壳尚未使用**，见 §5.4） |
| `CreationTaskStore`：日期区间增量、连续空页上限 5、游标未推进即停、页间节流 500ms（限速时 1500ms）、限速时挂起而非失败 | `crawl.run` 的 `strategy` + `done_reason`（`time_progressed` / `empty_pages` / `cursor_stuck` / `page_limit_reached`）。**这些判据组件已实现并测试** |
| `HomepageStore.fetchPage`：搜索/时间线双路 + "填满视口"循环 | 外壳用单页原语自己驱动。**这是有意的**：视口填充是渲染问题，不是爬取问题 |

---

## 5. 缺口清单

### 5.1 引用推文的正文 —— **已修（契约 1.4.0）**

- **参考实现**：`mapTwitterPost(_:includeQuoted:)` + `mapQuotedPost` 会从 `quoted_status_result`
  解析出被引用推文，界面据此渲染引用块（`HomeTimelineView` / `MediaDetailView` 都在用
  `post.quotedPost`）。
- **现状**：契约的 `post` / `reply` 只有 `quoted_id`（字符串），**没有内嵌正文、作者、媒体**；
  接入后 `XSpiderMapping` 恒为 `quotedPost: nil`。
- **后果**：引用推文在界面上**只剩一个空壳**（正文里那条 t.co 短链也没了）。
- **修法（已做）**：契约 1.4.0 给 `post` / `reply` 增加 `quoted`（`post | null`，只嵌一层、不递归），
  组件从 `quoted_status_result` 解析（两种包裹形状 + `legacy` 兜底），外壳映射到 `quotedPost`。
  **只增字段，不改不删**，符合兼容规则。离线有真实 fixture 用例，合成样本覆盖包裹形状与"只嵌一层"。

### 5.2 暂停/恢复与失败重试没有接线 —— **已修（外壳侧）**

- **现状**：外壳的"继续"与"自动重试"走的是 `start()` → `pump()` → `launch()` →
  **再次 `dl.enqueue`（同一个 `job_id`）**；而组件对已存在的 `job_id` 返回
  **`already_known` 且不会重启任务**（幂等是刻意的）。
- **后果**：功能上"暂停后继续"与"失败后重试"**都没有真正发生**——
  界面上任务显示"下载中"，组件里的任务仍是 `paused` / `error`，进度永远不动。
- **修法（已做）**：外壳把任务交回组件的那个唯一入口（`launch`）现在看 `dl.enqueue` 的
  `accepted_by`：命中 `already_known` 时补一次 **`dl.resume`**（对 `paused` 与 `error` 都有效，
  只拒绝 `complete`）。组件侧无需改动——**看返回值分支**，而不是新增一个"我在重试"的标记。
- **教训**：`already_known` 是幂等命中，**不是**"重新开始"。这条已写进
  [`07-API-REFERENCE.md`](07-API-REFERENCE.md) §4.4 与 §7.3。

### 5.3 重启后不与组件对账 —— **已修（外壳侧）**

外壳启动时把 `download-history.json` 里 `active` / `waiting` 的任务一律改成 `paused`，
而事件轮询只在"有新任务入队"时才启动。于是**只有恢复任务、没有新任务的那次启动，
不会去问组件的 `dl.list()`**，两边状态可能不一致（组件那边可能已经下完了）。
修法（已做）：启动时无条件跑一次对账，并且**真的把组件那边自动重新排队的任务暂停掉**——
组件读自己的记录做断点续传（`load_records` 会把未完成任务重新排队），而本应用的策略是
"重启不自动续传"（怕意外流量）。不补这一步，界面写着"已暂停"而字节在流。

### 5.4 `crawl.run` 未接进外壳 —— **已接（创建任务这一处）**

组件早就实现并测试了爬取循环（含终止判据族），而外壳仍自己写翻页循环。
**现在"创建任务"（`CreationTaskStore`）改走 `crawl.run`**：翻页、游标推进、
四条终止判据（到底 / 时间轴推进 / 连续空页 / 游标未推进）只有一份实现。

接线时按之前列的代价逐条处理：

| 代价 | 处理方式 |
|---|---|
| `crawl.run` 是"跑到停为止再返回"，进度看不见、取消不响应 | **切成小块**（`max_pages: 3`），块之间外壳更新进度、检查挂起与取消，用 `next_cursor` 续跑 |
| 限流时要能**挂起而不是失败**（外壳的 `shouldSuspendNewWork`） | 保留：每块**之前**先 `waitWhileThrottled()`，挂起期间 cursor 与进度都不动 |
| 组件按 **UTC 天**比较日期，用户选的是**本地日历** | 组件的 `since/until` **故意各放宽一天**当粗筛；精确边界（本地日历）由外壳按候选自带的 `created_at` 再做一次 |
| `media_types` 是**按推文**筛（"这条推文里有符合的"），旧实现是**按媒体**筛 | 两处都做：类型透传给组件当粗筛，外壳再逐条判 |
| 候选是**有损**的，算不出文件名、写不了历史记录 | **契约 1.5.0 给 `crawl.run` 加了 `posts[]`**（同一批推文的完整 DTO）。这是接真实外壳才暴露的缺口——第一个消费方是 CLI，它不要名字 |
| 契约的 `wanted_keys` 与外壳的键（`postId/mediaId`）不是一套 | **不用它**：两套键对不上时它静默不生效（不报错、只是多翻页），外壳自己判断"收齐即停"更直接 |

`SyncStore` **不接**（它按 `.synced.json` 的锚点日去重，是另一套产品语义），
`HomepageStore` 的"填满视口就停"也不接（那是渲染问题）。

### 5.5 `dl.prune` 未被调用 —— **无功能影响**

已结束的任务快照会一直留在组件内存里（数量级：一次会话几百条）。
调与不调不影响下载，只是内存。

顺带记一条容易被想反的语义：**`dl.prune` 只清内存快照，不清下载记录**。
配了 `--state-dir` 时记录还在，所以清完之后同一个 `job_id` 再入队**仍然**是
`already_known`（`enqueue` 在内存里查不到之后还会去查记录）。要重下必须换新的 `job_id`。

### 5.6 组件侧尚未实现 / 已知边界

| 项 | 状态 |
|---|---|
| `dl.plan` / `dl.report`（`host` 逃生舱） | 契约里已登记形状，**未实现**（调用得 `invalid_request`） |
| `net.set_limits.cdn_concurrency` 运行中不可变 | 信号量不能缩容；只在队列创建时生效 |
| `crawl.run` 的事件数组未逐字段约束 | schema 里是通用对象数组（`dl.events` 是逐字段约束的） |
| 推送式事件流 | 不做（三种形态统一用游标轮询，ADR-029） |

### 5.7 外壳里仍然直连 X CDN 的地方 —— **有意保留，不是缺口**

| 位置 | 用途 | 为什么不进组件 |
|---|---|---|
| `AccountStatusStore.probeCDN()` | 探一张 `pbs.twimg.com` 的图，判断 CDN 是否可用 | 是"健康探针"这种产品行为；组件侧对应的权威信息是 `net.status` 与错误的 `code` |
| `ImageCache` / `SidebarView` / `DownloadsView` | 头像与缩略图 | **图片展示**不属于契约范围（契约不覆盖 UI 取图），走 URLSession 直连更简单 |
| `CookieLoginSheet` | WKWebView 打开 `x.com/login` 取 cookie | 登录是外壳的事，cookie 由外壳注入 |

**已全部迁走、外壳里再无自建 HTTP 层的部分**：签名（`XClientTransaction`）、
limit/重试（`NetworkClient`、`RequestGate`）、queryId 自愈（`SearchQueryIdProvider`）、
aria2 引擎（`Aria2Engine`、`Aria2RPCClient`）——这些文件已从外壳删除。

---

## 6. 结论

**取数与写操作：一一对应，没有遗漏**（唯一例外是 §5.1 的引用推文正文）。

**下载：能力一一对应**，且有三处是刻意的改进（真实大小探测取代码率估算、
落盘字节数判据取代"引擎说成功"、断点按引擎隔离）。
外壳侧那两处没接线的问题（§5.2 暂停/恢复与重试、§5.3 重启对账）也已修。

**爬取：组件覆盖得比参考实现多**（终止判据族、`done_reason`），
且"创建任务"已经改走它（§5.4）——外壳侧那一套终止判据整段删掉了。

**有意留在外壳的**（分工，不是缺口）：文件名模板、目录选择、同文件跳过记录、
UI/通知、图片取用、魔数/HTML 误页判定、视口填充式滚动。
