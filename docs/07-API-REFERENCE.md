# 07 · 接口清单与调用方式（给外壳开发者）

> 这份是**照着就能写外壳**的那一份：全部 method 的入参/出参、三种形态怎么调、
> 怎么起 sidecar、错误怎么处理、哪些坑必须知道。
> 契约的"定义"在 [`CONTRACT.md`](CONTRACT.md) 与 [`xspider.schema.json`](../contract/xspider.schema.json)；
> 这里是"**怎么用**"。三份不一致时以 schema 为准（`crates/xspider-ffi/tests/contract_guard.rs` 会盯着）。

版本：契约 **1.1.0** · 构建 0.1.0 · 面 **23 个 method** + 1 个传输层 method

---

## 1. 三种形态：同一份载荷，三种叫法

| 形态 | 怎么调 | 什么时候用 |
|---|---|---|
| **sidecar + HTTP JSON-RPC**（主） | `POST http://127.0.0.1:<port>/`，带 `X-XSpider-Token` | 默认。换组件 = 换一个二进制，外壳不需要重新构建 |
| sidecar + stdio | 每行一个 JSON 请求，逐行读响应 | 不想要端口的场景（容器、管道） |
| cdylib | `xspider_call(method, json_in)` | 需要进程内调用，且**没有**开 hardened runtime |

**载荷在三种形态下完全一致**：

```json
// 请求
{ "id": 1, "method": "fetch.get_user", "params": { "screen_name": "jack" } }
// 成功
{ "id": 1, "result": { "user": { … } } }
// 失败
{ "id": 1, "error": { "code": "not_found", "message": "…" } }
```

- `id` 只是传输层流水号（stdio 用来配对），**不属于契约载荷**；
- cdylib 的 `xspider_call` 直接吃 `params`、吐出 `result`（错误时吐 `{"error":…}`）；
- **契约错误一律 HTTP 200**；HTTP 只表达传输层：`400` 请求体不是 JSON、`401` token 错、`404` 路径错。

### 怎么知道自己在哪种形态

`system.version` 的 `transport` 字段（`"sidecar"` / `"cdylib"`）。
**能力有差异**：`system.shutdown` 只有 sidecar 有，而它**不在** `system.methods` 里
（那份清单只列契约载荷）。所以：

```swift
// 有退出项才显示退出项，而不是硬编码"我是 sidecar"
let mayShutdown = version.transport == "sidecar"
```

---

## 2. 起一个 sidecar（外壳侧的最小实现）

```bash
xspiderd --port 0                 # 绑定随机端口
# stdout 打印一行后 flush（日志全在 stderr）：
ready {"port":49152,"token":"<随机>","version":"1.1.0","build":"0.1.0"}
```

必须遵守的六条：

1. **`--port 0` + 读 ready 行**拿到 `port`/`token`/`version`。不要用固定端口——
   多实例（App + CLI + 别的工具）会互踩；
2. **`token` 每次运行都不一样**，放在 `X-XSpider-Token` 头里。它不是身份，是"本机别的进程也别乱调"；
3. **读 stdout 的 ready 行才算握手完成**；标准输出只有这一行，别把日志混进去；
4. **退出时**：先 `system.shutdown`（优雅，落盘状态、结束子进程），
   5 秒没退再 `kill`。**不要**只 kill——那样下载记录与断点可能没落盘；
5. **崩溃自愈**：`xspiderd` 挂了就重起一个，然后用 `dl.list()` 与自己的记录**对账**
   （队列会从下载记录里恢复未完成任务，见 §4.2）；
6. **多实例要各自独立的 `--state-dir`**（下载记录 + 单实例锁都在那里）。

平台相关（macOS 实测）：

- 组件与 `aria2-next` 都要 **ad-hoc 签名**（`codesign -f -s -`），从浏览器下载来的还要
  `xattr -cr` 清隔离属性——否则内核直接 SIGKILL（137），表现为"静默不工作"；
- 启用 **hardened runtime** 的外壳**不要**用 cdylib 形态（`dlopen` 会被 library validation 拒），
  用 sidecar；
- 组件是独立进程，**父进程死了它也要死**：`xspiderd` 自己带
  `--stop-with-process` 等价的清理，但外壳仍应在退出时显式 `system.shutdown`。

---

## 3. method 清单（23 + 1）

### 3.1 系统（`system.*`）

| method | 入参 | 出参 | 说明 |
|---|---|---|---|
| `system.version` | `{}` | `{contract_version, build_version, transport}` | **握手**。主版本对不上就拒绝启动 |
| `system.methods` | `{}` | `{methods: [string]}` | 能力自检（启动时打一次日志很有用） |
| `system.shutdown` | `{}` | `{ok}` | **sidecar 独有**（传输层，不在上面的清单里） |

### 3.2 凭据与网络（`auth.*` / `net.*`）

| method | 入参 | 出参 | 说明 |
|---|---|---|---|
| `auth.set_cookie` | `{cookie, csrf?}` | `{ok}` | **只进不出**：不回显、不落盘、不打日志。缺 `ct0` 会被拒 |
| `auth.whoami` | `{}` | `{account}` | **登录校验**：页面里没有 `screen_name` 就是 cookie 失效 → `unauthorized`。替代"每个外壳自己抓首页正则" |
| `net.set_limits` | `{api_rps, api_burst, cdn_concurrency, cooldown_s}` | `{ok}` | 四个都必填。接口与 CDN 是**两套配额** |
| `net.set_proxy` | `{url}` | `{ok}` | `url: null` = 关闭代理；**字段不能缺**。运行中可换，取数与下载一起换 |
| `net.status` | `{}` | `{state, rate_limited_until?, retry_after_s?}` | 冷却**到期自然失效**，不需要谁来清除 |
| `net.probe_size` | `{url}` | `{size: int\|null}` | 问 CDN"这个文件多大"：先 `HEAD`，不行退到 1 字节 `Range`。`null` = 服务端没说；404 = `not_found` |

### 3.3 取数（`fetch.*`）

分页统一形状：`{items: [...], cursor?, end}`。**`cursor` 键不出现就是到头了**，`end` 同时为 `true`。

| method | 入参 | 出参 |
|---|---|---|
| `fetch.get_user` | `{screen_name}` | `{user}` |
| `fetch.user_medias` | `{user_id, cursor?, count?}` | `page<post>` |
| `fetch.user_tweets` | `{user_id, cursor?, count?, require_media?, include_retweets?}` | `page<post>` |
| `fetch.tweet_detail` | `{id}` | `{focal, replies[], cursor?}` |
| `fetch.search_timeline` | `{screen_name, since, until, media_only?, cursor?}` | `page<post>` |
| `fetch.home_timeline` | `{mode: "for_you"\|"following", cursor?}` | `page<post>` |
| `fetch.following` | `{user_id, cursor?, count?}` | `page<user>` |
| `fetch.is_following` | `{screen_name}` | `{following}` | 组件内部缓存"我是谁"，外壳不必传 `source_screen_name` |
| `fetch.mutate` | `{action, tweet_id?, screen_name?}` | `{ok}` | **写操作**。`action` ∈ favorite/unfavorite/retweet/unretweet/bookmark/unbookmark/follow/unfollow |

要点（每条都有实测依据，见 `docs/02`）：

- `count` 默认：时间线 20、关注列表 100；**`count: 0` 是非法值**；
- `cursor` **首页不要传**（连键都不要出现）——传空串不是"从头开始"，是错；
- `require_media` 默认 `true`（只要带媒体的推文）；被它筛空的页**不会**被判成"到底"；
- `search_timeline` 的 `since`/`until` 是**用户本地日历日期**（`YYYY-MM-DD`），`until` **含当天**，
  组件内部 +1 天适配 X 的排他语义——外壳不要再加一天；组件不做时区换算；
- `created_at` 是 **RFC3339 UTC**，缺失时是 `null`（那条推文仍然返回，不会丢）；
- 头像已经归一化（`https:` + `_bigger`），正文已经清洗（长推文用 `note_tweet`，短链换成真实 URL）。

**`post` 对象**：

```json
{ "id": "123…", "created_at": "2026-09-25T01:33:31Z", "full_text": "…",
  "lang": "en", "views": 4321, "favorite_count": 10, "retweet_count": 3, "reply_count": 1,
  "bookmark_count": 7, "quote_count": 2, "possibly_sensitive": false,
  "favorited": true, "retweeted": false, "bookmarked": true,   // 我的状态（按钮实心/空心）
  "medias": [ { "kind": "video", "id": "…", "url": "https://video.twimg.com/….mp4",          // 可下载/可播放
                "poster_url": "https://pbs.twimg.com/….jpg",      // 封面（界面拿它当图片）
                "ext": "mp4", "width": 1920, "height": 1080, "duration_ms": 61500,
                "aspect_ratio": [16,9], "variants": [ … ] } ],
  "author": { "id": "…", "screen_name": "…", "name": "…", "avatar": "https://…" },
  "tags": ["rust"], "quoted_id": "…", "in_reply_to_screen_name": "…", "in_reply_to_id": "…",
  "retweeted_by": { … } }
```

`media` 有**两个不同的 URL，别混**：

- `poster_url` = **封面**（X 的 `media_url_https`）：视频/动图是播放前的静帧，图片则是它本身。
  界面用它画格子、做封面——**把它当图片解码**。缺了它（早些版本没有这个字段）
  就会拿 mp4 去当图片解码，视频格子整片空白（真实故障，见 ADR-039）；
- `url` = **可直接下载的地址**：图片是 `media_url_https`（不含 `?name=`），
  视频/动图已选好最高码率、并过滤掉 HLS。下载与播放用它，**不要拿它当封面**。

需要缩略图/原图就自己拼 `?name=small` / `?name=orig`（`docs/02` §C6：`small` 是 680px）。

### 3.4 下载（`dl.*`）

```json
// 入队（job_id 由外壳生成，组件按它幂等）
{ "job_id": "post-1-media-2", "url": "https://…", "dest_dir": "/Users/me/dl/tesla",
  "file_name": "2026-09-25 01-33-31 tesla 123-1.mp4", "expect_size": 3122633,
  "requirements": { "resume": true, "segments": 4 }, "tag": "123", "skip_if_present": false }
// → { "accepted_by": "queued" }   // queued | already_known | skipped
```

| method | 入参 | 出参 |
|---|---|---|
| `dl.enqueue` | 见上 | `{accepted_by}` |
| `dl.pause` / `dl.resume` / `dl.cancel` | `{job_id}` | `{ok}` |
| `dl.status` | `{job_id}` | `{job}` |
| `dl.list` | `{}` | `{jobs: [jobSnapshot]}` |
| `dl.events` | `{since?}`（**第一次传 0**） | `{seq, events[]}` |
| `dl.prune` | `{}` | `{ok}` |

- **`job_id` 幂等**：同 id 第二次入队返回 `already_known`，**不发任何请求**。跨重启也有效；
- **目录与文件名由外壳算**：`dest_dir` + `file_name`，`file_name` **不许含路径分隔符**；
- **`requirements` 是能力表达，不是引擎名**：`segments > 1` = 希望多连接，派给谁由组件决定；
- **`expect_size` 强烈建议给**（`net.probe_size` 就能拿到）。不给也行——组件会自己探，
  代价是每个未知大小的媒体多一次 CDN 请求（`XSPIDER_PROBE_SIZE=0` 可关）；
- **`tag` 是不透明的**：组件只存不解释，外壳放什么都行（放推文 id 最省事）。

状态机：`waiting` → `active` → `complete` / `error`（`error.reason` 是结构化短标签；
**取消也走 `error` + `error.code == "cancelled"`，没有单独的 cancelled 状态**）。
暂停**保留断点**，取消**丢弃断点并清临时文件**。

事件（`dl.events`，形状见 §4.4）用**游标轮询**取增量：

```json
{ "since": 0 }  →  { "seq": 3, "events": [ {"kind":"progress", …}, {"kind":"completed", …} ] }
```

**节流是外壳的事**：进度事件按片发，多久刷新一次界面由你决定。

### 3.5 爬取（`crawl.run`）

```json
{ "source": "medias", "user_id": "13298072", "cursor": "…",
  "strategy": { "since": "2026-09-01", "until": "2026-09-30",
                "media_types": ["photo","video"], "wanted_keys": ["1234"],
                "limits": { "page_size": 20, "page_throttle_ms": 450,
                            "max_pages": 20, "empty_page_limit": 5 } } }
```

产出一份**候选清单**（`candidates[]`：url / ext / size_hint / day / screen_name）+ **`done_reason`**：

| `done_reason` | 含义 |
|---|---|
| `exhausted` | 服务端没有更多了 |
| `time_progressed` | 时间轴已推进到 `since` 之前（**主判据**） |
| `empty_pages` | 连续空页到上限 |
| `cursor_stuck` | 服务端回吐同一个游标 |
| `wanted_collected` | `wanted_keys` 收齐 |
| `page_limit_reached` | 触到 `max_pages`（**不是**服务端到底） |
| `cancelled` / `error` | 取消 / 出错 |

组件**只管筛出候选**，不下不命名；外壳拿到候选后自己决定下不下，再调 `dl.enqueue`。

---

## 4. 错误：只按 `code` 判断

```json
{ "error": { "code": "rate_limited", "message": "…", "retry_after_s": 120,
             "endpoint": "user_medias", "status": 429, "context": null,
             "detail": { "kind": "timeout" } } }
```

| `code` | 外壳该做什么 |
|---|---|
| `invalid_request` | **别重试**，是调用方的问题（拼错了字段/类型） |
| `unauthorized` | 提示重新登录，**别重试** |
| `rate_limited` | 展示倒计时（`retry_after_s`）；组件内部已挂起 |
| `not_found` | 展示"不存在"，别重试 |
| `upstream` | 看 `status`：5xx 可退避重试，4xx 先怀疑调用方式 |
| `parse` | **X 可能改版了**——这是最该报警的一类，把 `context` 报给组件作者 |
| `transport` | 网络/代理/DNS，`detail.kind` 说明类别（`timeout`/`connect`/`body`/`fixture`/`other`）。**组件内部已重试过** |
| `cancelled` | 正常路径，不是错误 |
| `internal` | 组件的 bug，报出来 |

三条纪律：

1. **绝不匹配 `message` 文案**——它是给人看的，改版即变；
2. **`parse` 要报警**（这是"X 改版了"的信号，不是"网络抖了"）；
3. **`transport` 可以自己再兜一层重试**：组件内部对单次请求重试 3 次（150ms/450ms 退避），
   但实测代理会出现**持续一两秒的整段拒连**。外壳用 1s/3s 这样的退避再试一次是合理的；
   **只重试 `transport`**，别把 `unauthorized` 也重试成三倍慢的 `unauthorized`。

---

## 5. 一份可以直接抄的调用时序

```
1. 起 sidecar（--port 0）→ 读 ready 行 → 拿到 port / token / version
2. system.version        → 主版本不匹配就拒绝启动（给可操作提示）
3. auth.set_cookie       → cookie 由外壳从 Keychain 读，只进不出
4. net.set_proxy         → 代理变了就再调一次（本机代理端口一天会变好几次）
5. net.set_limits        → 启动时一次（api_rps / api_burst / cdn_concurrency / cooldown_s）
6. fetch.get_user        → 拿 user_id
7. fetch.user_medias     → 一页 + 游标（要不要翻页由外壳决定）
8. net.probe_size × N    → 决定下不下、要不要多连接（探测失败按"未知"继续，别当错误）
9. dl.enqueue × N        → job_id 自己生成（建议 "<post_id>-<media_id>"，跨重启稳定）
10. dl.events(since) 轮询 + dl.list() → 画进度；**用 dl.list 判断"结束了没有"**
11. 收尾：dl.prune / system.shutdown
```

第 10 步为什么两个都用：事件是**增量日志**，用来画进度；而"结束了没有"必须问
`dl.list`/`dl.status`——外壳崩溃重连之后只有它是权威的。

---

## 6. 环境变量（只影响**进程内**行为，不是契约）

| 变量 | 作用 |
|---|---|
| `XSPIDER_COOKIE` | 启动时注入 cookie（等价于 `auth.set_cookie`）。**别写进文件** |
| `XSPIDER_PROXY` | 启动时的代理（等价于 `net.set_proxy {url}`） |
| `XSPIDER_FIXTURE_DIR` | 回放 fixture，**不发任何真实网络请求**（测试用） |
| `XSPIDER_STATE_DIR` | 下载记录 + 单实例锁的目录 |
| `XSPIDER_ARIA2_PATH` | Aria2Next 二进制路径。不配就只用内置 HTTP 后端 |
| `XSPIDER_PROBE_SIZE` | `0` = 不自动探测媒体大小（省 CDN 请求，代价是完整性只按"未知"处理） |
| `XSPIDER_LOG` | 日志级别（`debug` / `info` / `warn`）。日志一律走 **stderr** |

---

## 7. 两个"看起来该有但没有"的东西

- **写操作已经实现**（`fetch.mutate`，1.2.0 起）。三条纪律写在契约里：
  参数校验在发请求**之前**；失败**原样上报**（不吞、不谎报成功）；
  **成败看返回体**——X 对失败的突变返回的是 HTTP 200 + `errors[]`，
  只看状态码会给出"已点赞"的假象。
- **没有推送式事件流**。JSON-RPC 的请求/响应包络与 C ABI 都不支持流，
  所以统一用"带游标的增量轮询"（三种形态行为一致）；进程内形态另有 `subscribe()`。
