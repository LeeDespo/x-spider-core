# X-Spider 对外契约

> **这份文档是契约本身。** 先改这里，再改代码（铁律 1）。
> 机器可读版本：[`contract/xspider.schema.json`](../contract/xspider.schema.json)——
> 外壳不必读 Rust 代码，也不必读这份文档，读那份 schema 就能对接。
>
> 契约版本：**1.1.0**（由 `xspider_version()` 返回）

---

## 1. 入口形状（不可协商）

只有三个 C ABI 函数。C ABI 是唯一跨编译器、跨语言稳定的接口面。

```c
char* xspider_version(void);                            // "1.1.0"
char* xspider_call(const char* method, const char* json_in);  // 所有能力都走这一个入口
void  xspider_free(char* ptr);                          // 释放上面两个函数返回的字符串
```

- **新增能力 = 新增一个 method 字符串**（加法式演进，外壳无需改动）；
- 返回值内存由调用方拥有，**必须**用 `xspider_free` 释放；
- `xspider_call` 永不返回 NULL：任何异常都变成 JSON 错误包络；
- 参数一律当不可信：空指针、非 UTF-8、非法 JSON、未知 method 都有明确响应；
- `json_in` 允许为 NULL 或空串（等价于 `{}`），方便无参数方法；
  `method` 不允许 NULL（那一定是调用方的 bug）。

### 为什么是 JSON 边界，而不是生成的强类型绑定

1. **语言中立**：Swift / Kotlin / C# / Python / shell + curl 都能调；
2. **换组件最容易**：只要 method 名与字段不变，替换二进制就完成升级；
3. **可离线测**：契约测试就是「给定 json_in，断言 json_out」，不依赖真网络；
4. **不被生成器绑架**：绑定生成器（BoltFFI / UniFFI）只是**外壳侧的可选胶水**，
   不参与契约定义。用它们就锁定精确版本，且 ABI 只允许加法式变更。

代价要认：序列化开销、类型安全靠 schema + 契约测试维持、错误在运行期才暴露。

---

## 2. 三种传输形态

| 形态 | 怎么调 | 状态 |
|---|---|---|
| **sidecar + 本地 HTTP JSON-RPC** | `POST http://127.0.0.1:<port>/` | **主形态** |
| sidecar + stdio JSON Lines | 每行一个请求 / 一个响应 | 备选 |
| cdylib | 调 `xspider_call` | 次形态 |

**主形态是 sidecar 有实测依据**，不是偏好：一旦外壳启用 hardened runtime（公证的前提），
`dlopen` 任何 ad-hoc 签名的 dylib 都会被拒（`mapping process and mapped file have different Team IDs`），
而 sidecar 两个进程各自签名、互不验证。

### 2.1 请求与响应包络

HTTP 与 stdio 的请求体形状相同（`id` / `params` / `token` 都可省）：

```json
{ "id": 1, "method": "fetch.get_user", "params": { "screen_name": "jack" } }
```

响应：

```json
{ "id": 1, "result": { "user": { "id": "…", "screen_name": "…" } } }
{ "id": 1, "error": { "code": "rate_limited", "message": "…", "retry_after_s": 120 } }
```

- `id` 是**传输层**流水号（stdio 下用来配对；HTTP 下可选），**不属于契约载荷**；
- **契约载荷只有 `result` / `error` 两个键**；
- cdylib 形态的 `xspider_call` 返回的就是这个载荷本身
  （`json_in` 即上文的 `params`）。三种形态的载荷形状**完全一致**。

### 2.2 sidecar 握手

`--port 0` 时绑定随机端口，并在 **stdout** 打印一行后 flush：

```
ready {"port":49152,"token":"…","version":"1.1.0","build":"0.1.0"}
```

日志一律走 stderr。外壳读这一行即完成握手（并同时拿到契约版本）。
`--stdio` 模式下 stdout 只走 JSON Lines，ready 行改在 stderr 打印。

### 2.3 HTTP 状态码

**契约错误一律 200**——错误信息在包络里，不在 HTTP 状态里。
HTTP 状态只表达"传输层发生了什么"：

| 状态 | 含义 |
|---|---|
| 200 | 请求被理解并派发完成（`result` 或契约 `error`） |
| 400 | 请求体不是合法 JSON / 缺 `method` |
| 401 | token 缺失或不正确 |
| 404 | 路径不对（只有 `POST /`） |

---

## 3. method 一览

### 3.1 已实现（1.1.0）

| method | 请求 | 响应 |
|---|---|---|
| `system.version` | `{}` | `{contract_version, build_version, transport}` |
| `system.methods` | `{}` | `{methods: [string]}` |
| `auth.set_cookie` | `{cookie, csrf?}` | `{ok}` |
| `net.set_limits` | `{api_rps, api_burst, cdn_concurrency, cooldown_s}` | `{ok}` |
| `net.set_proxy` | `{url}` | `{ok}` |
| `net.status` | `{}` | `{state, rate_limited_until?, retry_after_s?}` |
| `net.probe_size` | `{url}` | `{size}` |
| `fetch.get_user` | `{screen_name}` | `{user}` |
| `fetch.user_medias` | `{user_id, cursor?, count?}` | `page<post>` |
| `fetch.user_tweets` | `{user_id, cursor?, count?, require_media?, include_retweets?}` | `page<post>` |
| `fetch.tweet_detail` | `{id}` | `{focal, replies[], cursor?}` |
| `fetch.search_timeline` | `{screen_name, since, until, media_only?, cursor?}` | `page<post>` |
| `fetch.home_timeline` | `{mode, cursor?}` | `page<post>` |
| `fetch.following` | `{user_id, cursor?, count?}` | `page<user>` |
| `dl.enqueue` | `{job_id, url, dest_dir, file_name, expect_size?, requirements?, tag?, skip_if_present?}` | `{accepted_by}` |
| `dl.pause` / `dl.resume` / `dl.cancel` | `{job_id}` | `{ok}` |
| `dl.status` | `{job_id}` | `{job}` |
| `dl.list` | `{}` | `{jobs[]}` |
| `dl.events` | `{since?}` | `{seq, events[]}` |
| `dl.prune` | `{}` | `{ok}` |
| `crawl.run` | `{source, user_id, cursor?, strategy?}` | `{done_reason, candidates[], pages, raw_items, dropped, next_cursor?, seq, events[]}` |

### 3.2 sidecar 传输层 method（不属于契约载荷）

| method | 说明 |
|---|---|
| `system.shutdown` | 优雅退出。仅 sidecar 有——cdylib 形态没有"关掉宿主进程"这回事。 |

**怎么知道自己在哪种形态**：`system.version` 的 `transport` 字段（`"sidecar"` / `"cdylib"`）。
它是为这一节存在的——上面这个 method **不在** `system.methods` 里（那份清单只列契约载荷），
所以外壳若想"有退出项才显示退出项"，就得问 `transport`，而不是硬编码"我是 sidecar"
（`docs/DECISIONS.md` ADR-037）。

### 3.3 已登记、**尚未实现**（M1，现在调用会得到 `invalid_request`）

> 列在这里是为了让外壳提前看到形状，**不代表现在可调用**。
> 按 `docs/00-KICKOFF.md` §4：垂直切片通过之前不铺开端点。

`fetch.mutate`（点赞/转推/书签）/ `dl.plan` / `dl.report`（`host` 逃生舱）

它们的形状见 `docs/01-ARCHITECTURE.md` §3–§5。

---

## 4. method 详情

### 4.1 `auth.set_cookie`

```json
// 请求
{ "cookie": "<外壳从 WebView / 浏览器拿到的完整 cookie 串>", "csrf": "可选，缺省时从 cookie 推导" }
// 响应
{ "ok": true }
```

- 凭据**只进不出**：不落盘、不打日志、不回传。响应里也不会回显任何凭据内容；
- 契约**不规定** cookie 里有哪些字段——那是 X 的实现细节，写进来就等于把它焊死；
- cookie 里必须有 `csrf`，或能被推导出的等价值，否则返回 `invalid_request`
  ——早失败，好过后面拿一个必然 403 的请求去排查；
- 重复调用即替换本次会话的凭据。

### 4.2 `net.set_limits`

```json
// 请求（四个字段都必填）
{ "api_rps": 2.0, "api_burst": 4, "cdn_concurrency": 4, "cooldown_s": 120 }
// 响应
{ "ok": true }
```

- `api_rps` / `api_burst`：接口端点的令牌桶（平均速率 / 突发额度）。
  默认值按上游"页间 400–500 ms"的节奏折算，Rust 循环必须显式等价出来；
- `cdn_concurrency`：媒体下载并发上限。**接口与 CDN 是两套配额，分别治理**；
- `cooldown_s`：收到限流但上游没给 `Retry-After` 时的默认冷却秒数。

### 4.3 `net.set_proxy`

```json
{ "url": "http://127.0.0.1:12450" }   // 指定代理
{ "url": null }                        // 关闭代理（忽略环境变量）
```

- `url` 字段**必须存在**：缺字段是漏参数，`null` 是明确"关闭"，两者语义不同；
- 未调用过本方法时，跟随标准环境变量（`HTTPS_PROXY` / `ALL_PROXY` / `NO_PROXY`）；
- 切换代理会导致签名密钥失效并重新握手（出口 IP 变了）。

### 4.4 `net.status`

```json
{ "state": "ok" }
{ "state": "rate_limited", "rate_limited_until": 1790791251, "retry_after_s": 42 }
```

- `rate_limited_until` 是 **Unix 秒（UTC）**；
- 冷却期**到期自然失效**，不需要任何人来"清除状态"——
  这一条是实测教训：断网时没有成功请求，若用粘性标志位，代理恢复后应用会一直卡着；
- **网络类异常不写入这里**：它只反映真实的限流，不反映"连不上"。

### 4.5 `net.probe_size`

```json
// 请求
{ "url": "https://video.twimg.com/…/…mp4" }
// 响应
{ "size": 15187101 }    // 字节数
{ "size": null }        // 服务端没说（**不是错误**）
```

- **为什么要问服务端**：GraphQL 的 media 对象里**没有字节数**（只有宽高、时长、码率），
  而 `码率 × 时长` 是编码器的**上限**、不是实际大小——实测一条视频估 76 MiB、
  实际 15.2 MB，差 **5.25 倍**（`docs/02` §E9）。CDN 直接给精确值；
- **组件只问不下载**：先 `HEAD`，被拒或不给长度时退到 1 字节的 `Range` 请求。
  一次调用最多产生一个字节的流量；
- **走 CDN 配额过闸门**（不是接口配额）：探大小也是流量，见 `docs/02` §B6；
- **`size: null` 的两种情况**：服务端没给 `Content-Length` / `Content-Range`，
  或者当前是**回放模式**（离线不联网，铁律 3）。外壳把它当"未知"处理即可；
- **404 是 `not_found` 错误**（不是 `null`）：这个媒体确实不在了，别下；
- **它是"可选优化"，不是下载的前置条件**：`dl.enqueue` 不传 `expect_size` 时
  组件自己也会探（ADR-032）。这个 method 是给外壳在**决定下不下之前**
  做体积判断用的（例如"超过 100 MB 才交给 Aria2Next"）。

### 4.6 `fetch.get_user`

```json
// 请求
{ "screen_name": "jack" }          // 允许带前导 @ 与空白，组件内部会归一化
// 响应
{ "user": {
    "id": "…",                      // 数字 id 的**字符串**形式（超出 f64 精度）
    "screen_name": "jack",
    "name": "Jack",
    "avatar": "https://…",
    "media_count": 12345,           // 可能为 null：缺失 ≠ 0
    "register_time": "2007-03-21T20:50:14Z"   // RFC3339 UTC，可能为 null
} }
```

- 这是**单页原语**：本端点不分页，所以请求里没有 cursor；
  将来的分页端点一律「单页 + 游标」，不提供自动翻页的流——
  翻页时机、过滤、去重是业务语义，必须让调用方看得见游标；
- `screen_name` 为空 → `invalid_request`；
- 用户不存在 → `not_found`（不是 `parse`，也不是空结果）。

### 4.7 分页约定（`fetch.user_medias` / `fetch.user_tweets` / `fetch.search_timeline` / `fetch.home_timeline`）

```json
// 请求：首页不要带 cursor —— **连这个键都不要出现**
{ "user_id": "13298072" }
// 请求：翻页时把上一页返回的游标原样传回
{ "user_id": "13298072", "cursor": "<上一页的 cursor>" }

// 响应
{ "items": [ { "id": "…", "full_text": "…", "medias": [ … ], "author": { … } } ],
  "cursor": "<下一页要传的游标>",
  "end": false }
```

- **单页 + 游标，不自动翻页**。翻页时机、过滤、去重是业务语义，必须让调用方看得见游标；
- **`cursor` 键不出现 = 服务端没有更多了**，此时 `end` 为 `true`；
  调用方**不要**把缺省的 `cursor` 当成 `null` 传回来（上游会把 `null` 当成"从头开始"，
  表现为无限重抓第一页）；
- 请求里 `cursor` 传空字符串等价于没传；
- `count` 缺省：推文类 20，`fetch.following` 100；
- **同一个 `id` 在一页里只会出现一次**（组件的去重先于筛选），
  重复转推不会让同一条推文出现两次。

### 4.8 `fetch.user_tweets` 的两个开关

| 字段 | 默认 | 含义 |
|---|---|---|
| `require_media` | `true` | 只要带媒体的推文。设 `false` 会保留纯文字推文 |
| `include_retweets` | `false` | 丢掉转推。设 `true` 时把**原推**提上来，并在 `retweeted_by` 里给出转推者 |

**注意**：`require_media=true` 时一页可能被筛成空 `items`——
那是**客户端筛选**的结果，**不是"到底"**。此时 `end` 仍为 `false` 且 `cursor` 仍会返回，
调用方应当继续翻页（`docs/02` §D5：到底判据只看服务端原始条数）。

### 4.9 `fetch.search_timeline` 的日期语义

- `since` / `until` 是 **`YYYY-MM-DD` 的日历日期**，按**用户本地日历**理解；
  组件**不做时区换算**（所以不存在"UTC 差一天"的坑）；
- **`until` 含当天**。上游区间语法是排他的，所以组件内部会 +1 天。
  **外壳不要再自己加一天**（重复加会让"至"当天之后多出一整天的内容）；
- `media_only` 默认 `true`：走服务端的媒体筛选，比取回来再筛**更省请求也更省配额**；
- 非法日期（如 `2026-02-30`）会在**发请求之前**被拒（`invalid_request`）。

### 4.10 `fetch.tweet_detail`

```json
{ "focal": { "id": "…", "full_text": "…" },
  "replies": [ { "id": "…", "full_text": "…", "parent_id": "…", "is_partial_parent": false } ] }
```

- `replies[]` 的元素把推文字段**摊平**在顶层（外壳处理回复与处理推文是同一套代码）；
- `focal` **不受媒体筛选影响**：无媒体的推文照样返回（否则详情里会弹出别人的推文）；
- `is_partial_parent = true` 表示**父推文不在本页**。这类条目**不会被丢弃**，
  外壳可据此决定是否补拉上下文；
- 广告条目已被过滤，`focal` 不会出现在 `replies` 里；
- 目前上游不返回详情的翻页游标，因此 `cursor` 通常不出现。

### 4.11 下载任务（`dl.*`）

```json
// 入队：job_id 由**外壳生成**，组件按它幂等
{ "job_id": "post-1-media-2", "url": "https://…", "dest_dir": "/Users/me/dl/tesla/2026-09-30",
  "file_name": "post-1-media-2.jpg", "expect_size": 204800,
  "requirements": { "resume": true, "segments": 1 }, "tag": "post-1" }
// 响应
{ "accepted_by": "queued" }   // queued | already_known | skipped
```

- **`job_id` 幂等**：同一个 id 第二次入队返回 `already_known`，**不发任何请求**。
  一次解决「跨页重复投递 / 暂停后重试 / 重启对账」三件事；
- **目录与文件名由外壳算好**：`dest_dir` + `file_name`。`file_name` **不许带路径分隔符**；
- **`requirements` 是能力表达，不是引擎名**：`segments > 1` 表示"希望多连接"，
  由组件决定派给哪个后端（引擎选择策略不暴露）；
- **`expect_size` 强烈建议给**：完成判据是**落盘字节数**，不是"后端说成功"
  （实测 Aria2Next 对 404 会报 complete 并留下 0 字节文件）；
- `tag` 是**不透明**的：组件只存不解释（它不可能知道 `TwitterPost` 是什么类型）；
- **`expect_size` 缺省时，组件会自己问服务端要**（`HEAD`，失败退回 1 字节的 `Range`）。
  所以"没给大小"不等于"无法校验"——拿到大小后照样做完整性校验。
  关掉它需要设置（见 `docs/DECISIONS.md` ADR-032）。

状态机（`dl.status` / `dl.list` 的 `state`）：

| 状态 | 含义 |
|---|---|
| `waiting` | 排队中（并发额度满了就在这里等） |
| `active` | 正在下 |
| `paused` | 被暂停，**断点保留**，`dl.resume` 接着下 |
| `error` | 结束但没成功。`reason` + `error.code` 说明原因（`cancelled` 也在这里） |
| `complete` | 下好且**校验通过**（或 `skipped`：文件已在且大小对） |

- **暂停 vs 取消**：暂停**保留**断点（`dl.resume` 会从断点继续）；
  取消**丢弃**断点并清掉临时文件（目标目录保持干净）；
- **重启对账**：组件把任务记录写在 `state-dir` 里（**带 `version` 字段**），
  重启后未完成的任务会自动重新排队续传；外壳用 `dl.list()` 与自己的记录对账；
- **记录是组件写的**（它是完成事件的产生者）——两个写者必然出现
  "文件下好了但记录没写"的静默不一致。

**事件（`dl.events`）用游标轮询取增量**，而不是推送流：

```json
{ "since": 0 }                          // 请求：上一次拿到的 seq；**第一次传 0**（0 是合法值）
{ "seq": 3, "events": [ … ] }           // 响应：下次把 seq 传回来
```

事件都用 `kind` 判别（机器可读形状见 schema 的 `$defs/downloadEvent`）：

```json
{ "kind": "progress",  "job_id": "…", "done": 1024, "total": 204800 }
{ "kind": "completed", "job_id": "…", "path": "…", "bytes": 204800,
  "integrity": { "result": "verified", "expected": 204800, "actual": 204800 } }
{ "kind": "failed",    "job_id": "…", "reason": "not_found", "error": { … } }
{ "kind": "skipped",   "job_id": "…", "reason": "already_present" }
```

- `progress.done` **单调不减**（续传时从断点开始）；
- `integrity` **只有两种形状**：`{result:"verified",expected,actual}`（服务端给过大小）
  与 `{result:"unverified",actual}`（没给过，**不能声称"校验通过"**，`docs/02` §E2）；
- `reason` 是**结构化短标签**（`cancelled` / `not_found` / `integrity_failed` / `truncated` /
  `transport` / `disk_full` / `upstream` / `invalid`…），按它做判断，**不要看文案**；
- **`since` 传 0 是合法的**。这条特意写在这里，因为它曾经是错的：实现把 0 当非法值挡掉了，
  而契约（本文）说的是"从 0 开始"——**契约与实现不一致时，错的是实现**
  （`docs/06-CONSUMER-INTEGRATION.md` §4.1）；
- **节流是调用方的事**：进度事件按片发，外壳自己决定多久刷新一次界面。

**为什么不是流**：JSON-RPC 的请求/响应包络与 C ABI 都不支持流；用"带游标的增量"
在三种形态下行为完全一致（进程内还能用 `subscribe()` 拿真流）。见 `docs/DECISIONS.md` ADR-029。

### 4.12 爬取（`crawl.run`）

```json
{ "source": "medias", "user_id": "13298072",
  "strategy": { "since": "2026-09-01", "until": "2026-09-30",
                "media_types": ["photo", "video"], "wanted_keys": ["1234"],
                "limits": { "page_size": 20, "page_throttle_ms": 450, "max_pages": 20,
                            "empty_page_limit": 5 } } }
```

- 产出**候选清单**（`candidates[]`：url / ext / size_hint / day / screen_name），
  外壳决定下不下、叫什么名、放哪儿，再调 `dl.enqueue`——**两个组件不直接对接**；
- **`done_reason` 必须显式**（见 §下表）。「翻到服务端尽头」与「被策略提前终止」
  是两种语义，外壳靠它决定文案与下次是否续爬；
- **`wanted_keys` 进契约、`excluded_keys` 不进**：前者改变翻页终止条件（省请求，成本相关）；
  后者只是产品语义，外壳在候选上过滤即可，零额外请求；
- **`limits.empty_page_limit` 只是辅助上限**（默认 5）。主终止判据是**时间轴推进**
  （`oldest_seen < since`）——否则停更一两个月的账号会被误判成"没有内容"；
- **没有 `created_at` 的推文放行**，不会因为解析不到时间而被丢掉；
- `until` **含当天**；客户端筛选按 **UTC 日期**比较（边界日与本地日历可能差一天，
  见 `docs/02` §D3）；
- 当前 `crawl.run` 是"跑到停为止再返回"（受 `max_pages` 约束）。
  长爬取请用小页数反复调用，用返回的 `next_cursor` 续爬。

| `done_reason` | 含义 |
|---|---|
| `exhausted` | 服务端没有更多了（游标为 null） |
| `time_progressed` | 客户端筛选的时间轴已推进到 `since` 之前（**主判据**） |
| `empty_pages` | 连续空页达到辅助上限 |
| `cursor_stuck` | 服务端回吐了同一个游标（防原地空转刷爆配额） |
| `wanted_collected` | `wanted_keys` 收齐，提前终止 |
| `page_limit_reached` | 触到调用方给的 `max_pages` 上限（**不是**服务端到底，也不是游标坏了） |
| `cancelled` | 调用方取消 |
| `error` | 出错终止 |

---

## 5. 错误契约

```json
{ "error": { "code": "rate_limited", "message": "…", "retry_after_s": 120, "endpoint": "user_by_screen_name" } }
```

| `code` | 何时出现 | 外壳该怎么办 |
|---|---|---|
| `invalid_request` | 未知 method、缺字段、字段类型错、参数不合法 | **不要重试**，是调用方的问题 |
| `unauthorized` | 凭据失效 / 被登出 / 未注入凭据 | 提示重新登录，**不要重试** |
| `rate_limited` | 触发限流，带 `retry_after_s` | 展示倒计时；组件内部已挂起，别催 |
| `not_found` | 用户/推文不存在 | 展示"不存在"，不要重试 |
| `upstream` | 上游 HTTP 错误，带 `status` | 5xx 可退避重试；404 要先分辨是不是调用方式问题 |
| `parse` | 解析失败 = **X 可能改版了** | 这是最该报警的一类；带上 `context` 报给我们 |
| `transport` | 网络/代理/DNS，`detail.kind` 说明类别 | 提示网络问题；已自动重试过 |
| `cancelled` | 调用方取消 | 正常路径，不是错误 |
| `internal` | 我们自己的 bug | 报 bug |

字段说明：

| 字段 | 类型 | 出现时机 |
|---|---|---|
| `code` | string enum | 总是 |
| `message` | string | 总是（**给人看**，不要用于判断） |
| `retry_after_s` | integer | 仅 `rate_limited` |
| `status` | integer | 仅 `upstream`（上游 HTTP 状态码） |
| `context` | string | 仅 `parse`（解析失败的位置） |
| `endpoint` | string | 逻辑端点名（如 `user_by_screen_name`）。**不是 URL 路径** |
| `detail` | object | 仅 `transport`：`{"kind": "timeout" \| "connect" \| "body" \| "fixture" \| "other"}` |

> **只按 `code` 做判断，不许匹配 `message` 文案。**
> 一个真实的反面教材：下载侧曾用匹配引擎输出文案（`"exit 3"` / `"404"`）来判断能否重试，
> 引擎一换就全错。

---

## 6. 版本与兼容

- 契约版本是语义化版本，由 `xspider_version()` 返回；
- 外壳启动时握手，**不匹配就拒绝启动并给明确提示**，不要降级到"部分功能可用"
  （未签名/被隔离的二进制表现为 137 静默死亡，是最难排查的失败模式）；
- 兼容规则：
  - **method 与字段只增不改不删**；
  - 要改语义就新增 method 名，旧名保留一个过渡期；
  - `code` 枚举值同样只增不改；
  - 每次变更同步更新 `contract/xspider.schema.json` 与契约测试的正反例；
  - 破坏性变更必须写 ADR（`docs/DECISIONS.md`）。

---

## 7. 契约里**绝不出现**的东西

一旦进去，契约就被焊死在今天的 X 上——而做这件事的全部意义就是不要被焊死：

- 端点路径与域名；
- queryId；
- `features` / `fieldToggles` 常量；
- HTTP 头名与头值（含任何凭据、Bearer、cookie 字段名）；
- Rust 类型名、结构体布局、泛型；
- 绑定生成器（BoltFFI / UniFFI）生成的类型。

自动化守卫：`crates/xspider-fetch/src/endpoints.rs` 里的测试会扫描本文件与
`contract/xspider.schema.json`，确保上面这些字面量没有漏出去。

---

## 8. 机器可读契约

[`contract/xspider.schema.json`](../contract/xspider.schema.json) 是 JSON Schema
（draft 2020-12），覆盖：

- 三种传输的请求/响应包络；
- 每个 method 的 `params` 与 `result`；
- 完整的 `error` 形状与 `code` 枚举（`additionalProperties: false`，防止拼错字段被静默忽略）。

`crates/xspider-ffi/tests/contract_guard.rs` 会断言 schema 里的 `method` 枚举
与代码里登记的 method **集合相等**——文档与实现不允许各说各话。
