# 07 · 接口参考（给外壳开发者）

> **这一册是"照着就能写外壳"的那一份**：全部 method 的入参/出参、三种形态怎么调、
> 怎么起 sidecar、错误怎么处理、数据长什么样、一份可以照抄的时序。
>
> 与另外两份的关系：
> [`CONTRACT.md`](CONTRACT.md) 是**契约定义**（规范条文，改动前先改它）；
> [`xspider.schema.json`](../contract/xspider.schema.json) 是**机器可读契约**（可以用工具校验）；
> 本册是**使用手册**（怎么用、怎么调、哪里容易错）。三者不一致时以 schema 为准，
> 契约守卫测试（`crates/xspider-ffi/tests/contract_guard.rs`）会盯着这个一致性。
>
> 本册不是开发日志：这里只写**当前版本的事实**。变更历史见 [`CHANGELOG.md`](../CHANGELOG.md)。

**版本**：契约 `1.3.0` · 构建 `0.1.0` · **26 个契约 method** + 1 个 sidecar 传输 method。

---

## 0. 全部 method 一览

| method | 入参（`*` = 必填） | 出参 | 一句话 |
|---|---|---|---|
| `system.version` | — | `{contract_version, build_version, transport}` | 握手、判断形态 |
| `system.methods` | — | `{methods[]}` | 能力自检 |
| `auth.set_cookie` | `*cookie`, `csrf` | `{ok}` | 注入凭据（只进不出） |
| `auth.whoami` | — | `{account}` | 登录校验 / 当前账号 |
| `net.set_limits` | `*api_rps`, `*api_burst`, `*cdn_concurrency`, `*cooldown_s` | `{ok}` | 限流参数 |
| `net.set_proxy` | `*url`（可为 `null`） | `{ok}` | 换/关代理（运行中可改） |
| `net.status` | — | `{state, rate_limited_until?, retry_after_s?}` | 限流状态 |
| `net.probe_size` | `*url` | `{size}` | 问 CDN 文件多大（≤1 字节流量） |
| `fetch.get_user` | `*screen_name` | `{user}` | 用户资料 |
| `fetch.user_medias` | `*user_id`, `cursor`, `count` | `page<post>` | 媒体时间线 |
| `fetch.user_tweets` | `*user_id`, `cursor`, `count`, `require_media`, `include_retweets` | `page<post>` | 推文时间线 |
| `fetch.tweet_detail` | `*id` | `{focal, replies[], cursor?}` | 推文详情 + 回复 |
| `fetch.search_timeline` | `*screen_name`, `*since`, `*until`, `media_only`, `cursor` | `page<post>` | 按日期搜某人的推文 |
| `fetch.home_timeline` | `*mode`, `cursor` | `page<post>` | 主页时间线（推荐/关注） |
| `fetch.following` | `*user_id`, `cursor`, `count` | `page<user>` | 关注列表 |
| `fetch.is_following` | `*screen_name` | `{following}` | 关注态（每张卡片都会问） |
| `fetch.mutate` | `*action`, `tweet_id`, `screen_name` | `{ok}` | **写操作**：赞/转推/书签/关注 |
| `dl.enqueue` | `*job_id`, `*url`, `*dest_dir`, `*file_name`, `expect_size`, `requirements`, `tag`, `skip_if_present` | `{accepted_by}` | 入队下载 |
| `dl.pause` / `dl.resume` / `dl.cancel` | `*job_id` | `{ok}` | 暂停（留断点）/ 恢复 / 取消（丢断点） |
| `dl.status` | `*job_id` | `{job}` | 单个任务快照 |
| `dl.list` | — | `{jobs[]}` | 全部任务快照（重启对账的权威来源） |
| `dl.events` | `since` | `{seq, events[]}` | 增量事件（游标轮询） |
| `dl.prune` | — | `{ok}` | 清掉已结束的任务 |
| `crawl.run` | `*source`, `*user_id`, `cursor`, `strategy` | `{done_reason, candidates[], …}` | 跑一轮爬取，产出候选清单 |
| `system.shutdown` | — | `{ok}` | 优雅退出（**仅 sidecar**，不在上面 26 个里） |

对象形状（`page<post>`、`job`、`event` …）见 §5；错误见 §6。

---

## 1. 五分钟上手

### 1.1 起 sidecar 并调一次

```bash
$ xspiderd --port 0
ready {"port":49152,"token":"…","version":"1.3.0","build":"0.1.0"}     # ← stdout，只有这一行

# 另开一个终端（port / token 用上面这一行里的）
$ curl -s http://127.0.0.1:49152/ \
    -H 'X-XSpider-Token: …' -H 'Content-Type: application/json' \
    -d '{"id":1,"method":"fetch.get_user","params":{"screen_name":"jack"}}'
{"id":1,"result":{"user":{"id":"12","screen_name":"jack","name":"jack","avatar":"https://…"}}}
```

日志**一律走 stderr**，stdout 只用于 ready 行——外壳直接读 stdout 的第一行即可握手。

### 1.2 载荷形状（三种形态完全一致）

```json
// 请求
{ "id": 1, "method": "fetch.get_user", "params": { "screen_name": "jack" } }
// 成功
{ "id": 1, "result": { "user": { … } } }
// 失败（**HTTP 状态仍是 200**，错误在包络里）
{ "id": 1, "error": { "code": "not_found", "message": "…" } }
```

- `id` 只是传输层流水号（stdio 下用来配对请求与响应）；**契约载荷只有 `result` / `error` 两个键**；
- cdylib 形态的 `xspider_call(method, json_in)` 直接吃 `params`、吐 `result`
  （失败时吐 `{"error": …}`）——所以同一份解析代码可以复用；
- `params` 必须是对象。无参数的 method 传 `{}`；**传 `null` 会被拒**（`invalid_request`）。

### 1.3 三种形态与选择

| 形态 | 怎么调 | 什么时候用 |
|---|---|---|
| **sidecar + HTTP JSON-RPC**（主） | `POST http://127.0.0.1:<port>/` + `X-XSpider-Token` | 默认。换组件 = 换一个二进制，外壳不必重新构建 |
| sidecar + stdio | 每行一个 JSON 请求，逐行读响应 | 不想要端口的场景（容器、管道） |
| cdylib | `xspider_call(method, json_in)` | 需要进程内调用，且外壳**没有**开 hardened runtime |

**怎么知道自己在哪种形态**：`system.version` 的 `transport` 字段（`"sidecar"` / `"cdylib"`）。
能力有差异——`system.shutdown` 只有 sidecar 有，而且它**不在** `system.methods` 里
（那份清单只列契约载荷）。所以判断方式应该是：

```swift
let mayShutdown = version.transport == "sidecar"   // 而不是硬编码"我是 sidecar"
```

### 1.4 启动自检（建议每个外壳都做）

```
1. system.version   → 主版本不同就拒绝启动，并给可操作提示（不要降级成"部分功能可用"）
2. system.methods   → 我依赖的 method 是否都在？缺了就是组件版本太旧，打一条明确的日志
```

---

## 2. sidecar 的生命周期（外壳要做什么）

### 2.1 启动与握手

```bash
xspiderd --port 0                        # 绑定随机端口，stdout 打印一行 ready JSON
```

必须遵守的六条：

1. **用 `--port 0` + 读 ready 行**拿 `port` / `token` / `version`。不要用固定端口——
   多实例（App + CLI + 别的工具）会互踩；
2. **`token` 每次运行都不同**，放在 `X-XSpider-Token` 头里。它不是身份，是"本机别的进程也别乱调"。
   不要打日志；
3. **stdout 只有 ready 那一行**：别把别的输出混进去，也不要依赖 stderr 的解析；
4. **退出时先 `system.shutdown`**（优雅：落盘状态、结束后端子进程），5 秒没退再 `kill`。
   只 kill 的话，下载记录与断点可能没落盘；
5. **多实例各自独立的 `--state-dir`**：下载记录与单实例锁都在那里，共用会互相踩；
6. **组件自带父进程看门狗**：外壳被强杀（SIGKILL、调试器停进程、测试宿主被收走）时，
   `xspiderd` 会自己退出，不留孤儿。但这只是兜底——正常路径仍应显式关停。

### 2.2 崩溃自愈

`xspiderd` 挂了就重起一个，然后用 `dl.list()` 与外壳自己的记录**对账**
（队列会从 `--state-dir` 的记录里恢复未完成任务）。

### 2.3 平台注意（macOS 实测结论）

- 组件与 `aria2next` 都要 **ad-hoc 签名**（`codesign -f -s -`）；从浏览器下载来的还要先
  `xattr -cr` 清隔离属性。**两步都不能省**：带隔离属性的可执行文件会被系统直接杀掉
  （退出码 137、没有任何输出），表现是"组件静默不工作"；
- **启用 hardened runtime 的外壳不要用 cdylib 形态**（`dlopen` 会被 library validation 拒），
  用 sidecar——那时两个进程各自签名、互不验证；
- 想用"应用与组件分开升级"的部署方式：**应用优先读外部目录里的组件**，bundle 内的只作兜底。
  换组件 = 换掉那个目录里的两个文件 + 重新签名，不必重新构建应用。

### 2.4 环境变量（只影响进程内行为，**不是契约**）

| 变量 | 作用 |
|---|---|
| `XSPIDER_COOKIE` | 启动时注入 cookie（等价于 `auth.set_cookie`）。**别写进文件** |
| `XSPIDER_PROXY` | 启动时的代理（等价于 `net.set_proxy {url}`） |
| `XSPIDER_STATE_DIR` | 下载记录 + 单实例锁的目录。**多实例必须各不相同** |
| `XSPIDER_ARIA2_PATH` | Aria2Next 二进制路径。不配就只用内置 HTTP 后端（少多连接） |
| `XSPIDER_PROBE_SIZE` | `0` / `false` / `off` = 不自动探测媒体大小（省 CDN 请求，代价是完整性只按"未知"处理） |
| `XSPIDER_FIXTURE_DIR` | **测试专用**：从 fixture 回放，不发任何真实网络请求 |
| `XSPIDER_LOG` | 日志级别（`debug` / `info` / `warn`）。日志一律走 **stderr** |

---

## 3. cdylib 形态

```c
char* xspider_version(void);                                  // "1.3.0"
char* xspider_call(const char* method, const char* json_in);  // 所有能力
void  xspider_free(char* ptr);                                // 释放上面两个函数返回的字符串
```

- `json_in` 是被调 method 的 `params`（允许 `NULL` 或空串，等价 `{}`）；`method` 不允许 `NULL`；
- **返回值内存归调用方**，必须 `xspider_free`——不释放就是内存泄漏；
- `xspider_call` **永不返回 NULL**：任何异常都变成 JSON 错误包络；
- cdylib **没有** `system.shutdown`（没有"关掉宿主进程"这回事），
  也没有推送式事件流（`dl.events` 照常轮询，见 §4.4）。

---

## 4. method 参考

约定：**「必填」列为 `是` 的字段，缺了会在发请求之前就被拒**，错误里会点名是哪个字段。
所有 ID 都是**字符串**（数字 id 超出 f64 精度，当数字传会丢精度）。

### 4.1 系统（`system.*`）

#### `system.version` — 握手

**入参** `{}`

**出参**

```json
{ "contract_version": "1.3.0", "build_version": "0.1.0", "transport": "sidecar" }
```

| 字段 | 说明 |
|---|---|
| `contract_version` | 契约版本。**握手看它**（主版本不同就拒绝启动） |
| `build_version` | 组件构建版本。排障用，不参与握手 |
| `transport` | `"sidecar"` \| `"cdylib"`。能力差异由此判断（§1.3） |

#### `system.methods` — 能力自检

**入参** `{}` → **出参** `{ "methods": ["fetch.get_user", …] }`

返回的是**契约 method**（不含 `system.shutdown`）。建议在启动时打一条日志，
排查"为什么这个方法不存在"时一眼看到组件版本。

#### `system.shutdown` — 优雅退出（**仅 sidecar**）

**入参** `{}` → **出参** `{ "ok": true }`

先调它，等 5 秒，再 `kill`。

---

### 4.2 凭据与网络（`auth.*` / `net.*`）

#### `auth.set_cookie` — 注入凭据

**入参** `{ "cookie": "<完整 cookie 串>", "csrf": "<可选>" }` → **出参** `{ "ok": true }`

- **只进不出**：不落盘、不打日志、不回传；响应里也不会回显任何凭据内容。
  外壳从 WebView / Keychain 拿到 cookie 后调一次即可；
- 契约**不规定** cookie 里有哪些字段（那是 X 的实现细节）；
- cookie 里必须能推出 `csrf`（`ct0`），否则返回 `invalid_request`——**早失败**，
  好过后面拿一个必然 403 的请求去排查；
- 重复调用即替换本次会话的凭据，并**自动失效组件内部缓存的"我是谁"**（`fetch.is_following` 依赖它）。

#### `auth.whoami` — 登录校验 / 当前账号

**入参** `{}`

**出参** `{ "account": { "screen_name": "jack", "avatar": "https://…", "id": "12" } }`

| 字段 | 必填 | 说明 |
|---|---|---|
| `screen_name` | 是 | 当前登录账号 |
| `avatar` | 是 | 已归一化的头像 URL |
| `id` | 否 | 数字 id 字符串；首页 HTML 里偶尔没有，缺失为 `null`（需要时再用 `fetch.get_user` 补） |

- **它是登录校验**：cookie 失效时报 `unauthorized`（不是 `parse`）；
- 一次调用会抓一次 x.com 首页——**别在热路径里反复调**；
- 它替代了此前每个外壳都要自己写的"抓首页 + 正则"。

#### `net.set_limits` — 限流参数

**入参**（四个都必填）

```json
{ "api_rps": 2.0, "api_burst": 4, "cdn_concurrency": 4, "cooldown_s": 120 }
```

| 字段 | 类型 | 约束 | 说明 |
|---|---|---|---|
| `api_rps` | number | > 0 | 接口端点平均速率（请求/秒） |
| `api_burst` | integer | ≥ 1 | 接口端点突发额度 |
| `cdn_concurrency` | integer | ≥ 1 | 媒体下载并发上限 |
| `cooldown_s` | integer | ≥ 1 | 上游没给 `Retry-After` 时的默认冷却秒数 |

**接口与 CDN 是两套配额，分开治理**。`cdn_concurrency` 目前只在下载队列创建时生效
（信号量不能缩容），所以**启动时调一次**即可。

#### `net.set_proxy` — 运行中换代理

**入参** `{ "url": "http://127.0.0.1:17890" }` / `{ "url": null }` → **出参** `{ "ok": true }`

- `url` 字段**必须存在**：缺字段是漏参数（`invalid_request`），`null` 是明确的"关闭代理"（忽略环境变量），
  两者语义不同；
- 从未调用过时，跟随标准环境变量（`HTTPS_PROXY` / `ALL_PROXY` / `NO_PROXY`）；
- **取数与下载一起换**，不必重启进程、不必重建队列；
- 切换代理会让签名密钥失效并重新握手（出口 IP 变了），属正常现象；
- **代理是会变的**：实测本机代理端口一天内变了好几次，且整段时间不可达。
  外壳应当把"代理设置变化"实时推给它，而不是只在启动时传一次。

#### `net.status` — 限流状态

**入参** `{}`

**出参**

```json
{ "state": "ok" }
{ "state": "rate_limited", "rate_limited_until": 1790791251, "retry_after_s": 42 }
```

- `rate_limited_until` 是 **Unix 秒（UTC）**；
- 冷却期**到期自然失效**——没有任何"需要人工清除"的状态；
- **网络类异常不写入这里**：它只反映真实的限流，不反映"连不上"。断网不会被误报成限流。

#### `net.probe_size` — 问 CDN 这个文件多大

**入参** `{ "url": "https://video.twimg.com/…/x.mp4" }`

**出参** `{ "size": 15187101 }` / `{ "size": null }`

- 一次调用最多产生 **1 个字节**的流量（先 `HEAD`，被拒或不给长度时退到 1 字节的 `Range`）；
- `size: null` = 服务端没说（**不是错误**）。回放模式下恒为 `null`；
- **404 是 `not_found` 错误**（不是 `null`）：这个媒体确实不在了，别下；
- 走的是 **CDN 配额**（探大小也是流量），不是接口配额；
- 用途：在**决定下不下之前**按体积过滤（例如"超过 100 MB 才交给 Aria2Next"）。
  它是可选优化——`dl.enqueue` 不传 `expect_size` 时组件自己也会探。

---

### 4.3 取数（`fetch.*`）

**分页统一形状**：`{ items: [...], cursor?, end }`。
**`cursor` 键不出现 = 服务端没有更多了**，此时 `end` 为 `true`。

**所有分页 method 都是"单页 + 游标"，不会替你翻页。** 翻页时机、过滤、去重是业务语义，
必须让调用方看得见游标。

#### `fetch.get_user` — 用户资料

**入参** `{ "screen_name": "jack" }`（允许带前导 `@` 与空白，组件内部归一化）

**出参** `{ "user": { … } }`（形状见 §5.1）

- 本端点**不分页**，所以请求里没有 cursor；
- 用户不存在 → `not_found`（不是 `parse`，也不是空结果）。

#### `fetch.user_medias` — 媒体时间线

| 字段 | 类型 | 必填 | 默认 | 说明 |
|---|---|---|---|---|
| `user_id` | string | 是 | — | 用户的数字 id（来自 `fetch.get_user`） |
| `cursor` | string | 否 | — | 上一页返回的游标。**首页不要传**（连键都不要出现） |
| `count` | integer | 否 | 20 | 每页条数，必须 ≥ 1 |

**出参** `page<post>`（§5.4）

#### `fetch.user_tweets` — 推文时间线

| 字段 | 类型 | 必填 | 默认 | 说明 |
|---|---|---|---|---|
| `user_id` | string | 是 | — | 同上 |
| `cursor` | string | 否 | — | 同上 |
| `count` | integer | 否 | 20 | ≥ 1 |
| `require_media` | boolean | 否 | **`true`** | 只要带媒体的推文。设 `false` 会保留纯文字推文 |
| `include_retweets` | boolean | 否 | **`false`** | 丢掉转推。设 `true` 时把**原推**提上来，并在 `retweeted_by` 里给出转推者 |

> **`require_media: true` 时一页可能被筛成空 `items`**——那是**客户端筛选**的结果，
> **不是"到底"**。此时 `end` 仍为 `false` 且 `cursor` 仍会返回，应当继续翻页。

#### `fetch.tweet_detail` — 推文详情 + 回复

**入参** `{ "id": "<推文 id>" }` → **出参** `{ "focal": post, "replies": [reply], "cursor"? }`（§5.5）

- `focal` 是**被点开的那条**，**不受媒体筛选影响**（无媒体的推文照样返回）；
- `replies[]` 的元素把推文字段**摊平**在顶层（处理回复与处理推文是同一套代码），
  另加 `parent_id` 与 `is_partial_parent`；
- **`is_partial_parent: true` 表示父推文不在本页**（孤儿）。这类条目**不会被丢弃**，
  外壳据此决定是否补拉上下文；
- 广告条目已过滤，`focal` 不会出现在 `replies` 里；
- 上游目前不返回详情的翻页游标，因此 `cursor` 通常不出现。

#### `fetch.search_timeline` — 按日期搜某人的推文

| 字段 | 类型 | 必填 | 默认 | 说明 |
|---|---|---|---|---|
| `screen_name` | string | 是 | — | 允许带前导 `@` |
| `since` | string | 是 | — | `YYYY-MM-DD`，**用户本地日历日期**，含当天 |
| `until` | string | 是 | — | `YYYY-MM-DD`，**含当天** |
| `media_only` | boolean | 否 | **`true`** | 走服务端的媒体筛选（比取回来再筛更省请求、更省配额） |
| `cursor` | string | 否 | — | 同上 |

- 组件**不做时区换算**（所以不存在"UTC 差一天"的坑）：它把日期当**用户本地日历**理解；
- 上游区间语法是排他的，组件内部会 **+1 天**——**外壳不要再自己加一天**；
- 非法日期（如 `2026-02-30`）在**发请求之前**被拒（`invalid_request`）。

#### `fetch.home_timeline` — 主页时间线

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `mode` | string | 是 | `"for_you"`（推荐）\| `"following"`（关注）。非法值在发请求前被拒 |
| `cursor` | string | 否 | 同上 |

#### `fetch.following` — 关注列表

| 字段 | 类型 | 必填 | 默认 | 说明 |
|---|---|---|---|---|
| `user_id` | string | 是 | — | 谁的关注列表 |
| `cursor` | string | 否 | — | 同上 |
| `count` | integer | 否 | **100** | 每页条数（注意与推文类端点的 20 不同） |

**出参** `page<user>`（§5.4）

#### `fetch.is_following` — 关注态

**入参** `{ "screen_name": "jack" }` → **出参** `{ "following": true }`

- **这是每张推文卡都会问一次的**，所以组件内部缓存了"我是谁"
  （换 cookie 时自动失效）——外壳不必自己传 `source_screen_name` 这类实现细节；
- **`false` 与"没查到"是两件事**：结构对不上时组件报 `parse`（那是"X 改版了"），
  而不是默默返回 `false`——后者会让界面显示错误的关注状态。

#### `fetch.mutate` — 写操作

**入参**

```json
{ "action": "favorite",   "tweet_id": "123…" }
{ "action": "follow",     "screen_name": "jack" }
```

| `action` | 需要哪个参数 |
|---|---|
| `favorite` / `unfavorite` / `retweet` / `unretweet` / `bookmark` / `unbookmark` | `tweet_id` |
| `follow` / `unfollow` | `screen_name` |

**出参** `{ "ok": true }`

- **动的是用户的真实账号**，所以三条纪律是硬性的：
  1. 参数校验在发请求**之前**（缺字段不会发出半个请求）；
  2. 失败**原样上报**（不吞、不谎报成功）；
  3. **成败看返回体**——X 对失败的突变返回的是 HTTP 200 + `errors[]`，
     只看状态码会给出"已点赞"的假象。组件已经把这一层处理掉了，外壳按 `code` 判断即可。
- 关注类动作成功后，组件会失效关注态缓存；外壳侧若有自己的缓存也要同步失效。

---

### 4.4 下载（`dl.*`）

#### `dl.enqueue` — 入队

```json
{ "job_id": "post-1-media-2",
  "url": "https://video.twimg.com/…/x.mp4",
  "dest_dir": "/Users/me/dl/tesla/2026-09-30",
  "file_name": "2026-09-30 12-34-56 tesla 123-1.mp4",
  "expect_size": 3122633,
  "requirements": { "resume": true, "segments": 4 },
  "tag": "123",
  "skip_if_present": false }
```

| 字段 | 类型 | 必填 | 默认 | 说明 |
|---|---|---|---|---|
| `job_id` | string | 是 | — | **外壳生成的幂等键**。建议 `<post_id>-<media_id>`，跨重启稳定 |
| `url` | string | 是 | — | 要下的地址（`media.url`） |
| `dest_dir` | string | 是 | — | 目录由外壳算好 |
| `file_name` | string | 是 | — | **只是文件名**，不许含路径分隔符 |
| `expect_size` | integer | 否 | — | 期望字节数，≥ 0。给了就**必须校验通过才算完成** |
| `requirements.resume` | boolean | 否 | `true` | 要不要断点续传 |
| `requirements.segments` | integer | 否 | — | 1–16。`> 1` = "希望多连接"（**能力表达，不是引擎名**） |
| `tag` | string | 否 | — | **不透明**标记，组件只存不解释（放推文 id 最省事） |
| `skip_if_present` | boolean | 否 | `false` | 目标文件已在且大小对就跳过 |

**出参** `{ "accepted_by": "queued" }`，取值：

| 值 | 含义 | 外壳该做什么 |
|---|---|---|
| `queued` | 已入队 | 开始轮询进度 |
| `already_known` | **同 `job_id` 已存在**（幂等命中），没有发任何请求 | 见下面的重要提示 |
| `skipped` | 目标文件已在且大小对 | 直接标成本地已完成 |

> ⚠️ **`already_known` 不会重启任务。** 同一个 `job_id` 第二次入队只算一次，
> 跨重启也有效（记录里有也算）。所以：
> **恢复一个暂停/失败的任务要用 `dl.resume`，不是再 `dl.enqueue` 一次**——
> 后者只会得到 `already_known`，任务原地不动。
> （这条是真实的坑：有外壳把"暂停后恢复"实现成重新入队，结果界面显示"下载中"而进度永远不动。）

要点：

- **`expect_size` 强烈建议给**（`net.probe_size` 就能拿到）。完成判据是**落盘字节数**，
  不是"后端说成功"——实测 Aria2Next 对 404 会报 `complete` 并留下 0 字节文件；
- 不给 `expect_size` 也行：组件会在下载前自己探一次（每个未知大小的媒体多一次 CDN 请求）；
- **跨页重复投递 / 暂停后重试 / 重启对账**三件事，靠 `job_id` 幂等一次解决。

#### `dl.pause` / `dl.resume` / `dl.cancel` — 任务控制

**入参** `{ "job_id": "…" }` → **出参** `{ "ok": true }`

| method | 语义 |
|---|---|
| `dl.pause` | **保留断点**，`dl.resume` 会从断点接着下 |
| `dl.resume` | 恢复。**对 `paused` 与 `error` 都有效**；只有 `complete` 会被拒 |
| `dl.cancel` | **丢弃断点**并清掉临时文件（目标目录保持干净） |

- 三者对未知 `job_id` 都返回 `not_found`；对已 `complete` 的任务，pause/resume/cancel 都会被拒
  （`invalid_request`）；
- 取消是**异步**的：在飞的任务由"真正停下来的那一刻"落终态，所以取消后立刻查
  `dl.status` 可能仍是 `active`——这不是没生效。

#### `dl.status` / `dl.list` — 查任务

- `dl.status { "job_id": "…" }` → `{ "job": jobSnapshot }`；未知 id → `not_found`；
- `dl.list {}` → `{ "jobs": [jobSnapshot] }`（按 `job_id` 排序，外壳做 diff 时不用自己排）。

`jobSnapshot` 形状见 §5.6。**"结束了没有"必须问它们**——事件流是增量日志，
外壳崩溃重连之后只有 `dl.list` 是权威的。

#### `dl.events` — 增量事件

**入参** `{ "since": 0 }`（**第一次传 0**；`0` 是合法值）→ **出参** `{ "seq": 3, "events": [...] }`

- 下次把上一次响应里的 `seq` 传回来即可取增量；
- **`since` 传 0 = 从头取**。这条特意写在这里，因为它曾经是错的（实现把 0 当非法值挡回，
  而契约说的是"从 0 开始"）——**契约与实现不一致时，错的通常是实现**；
- 事件用 `kind` 判别：`progress` / `completed` / `failed` / `skipped`（形状见 §5.6）；
- **节流是调用方的事**：进度事件按片发，外壳自己决定多久刷新一次界面；
- 事件是**增量日志**，适合画进度；"到底结束了没有"用 `dl.list`。

#### `dl.prune` — 清掉已结束的任务

**入参** `{}` → **出参** `{ "ok": true }`

清掉内存里 `complete` / `error` 的任务快照。调不调都行（不调只是内存里多留几条）。
注意：**清掉之后，同 `job_id` 重新入队就会真的重新开始下载**——这是"重下"的一种实现方式，
但正常的重下请用新的 `job_id` 或 `dl.resume`。

---

### 4.5 爬取（`crawl.run`）

跑一轮爬取循环，**产出候选清单**（下不下、叫什么名、放哪儿由外壳决定）。

```json
{ "source": "medias",
  "user_id": "13298072",
  "cursor": "…",
  "strategy": {
    "since": "2026-09-01", "until": "2026-09-30",
    "media_types": ["photo", "video"],
    "wanted_keys": ["1234"],
    "limits": { "page_size": 20, "page_throttle_ms": 450, "max_pages": 20,
                "empty_page_limit": 5, "stop_when_older_than": "2026-08-01" } } }
```

| 字段 | 必填 | 说明 |
|---|---|---|
| `source` | 是 | `"medias"`（媒体时间线）\| `"tweets"`（推文时间线，自动只取带媒体的、丢转推） |
| `user_id` | 是 | 谁的 |
| `cursor` | 否 | 从哪儿续爬（上一轮返回的 `next_cursor`） |
| `strategy.since` / `until` | 否 | `YYYY-MM-DD`；`until` **含当天**。客户端筛选按 **UTC 日期**比较 |
| `strategy.media_types` | 否 | `photo` / `video` / `animated_gif` 的子集 |
| `strategy.wanted_keys` | 否 | 收齐即停（**它进契约是因为它会改变翻页终止条件 = 省请求**） |
| `strategy.limits.page_size` | 否 | 每页条数 |
| `strategy.limits.page_throttle_ms` | 否 | 页间节流 |
| `strategy.limits.max_pages` | 否 | 本轮最多翻几页 |
| `strategy.limits.empty_page_limit` | 否 | **辅助**上限，默认 5（别当主终止判据） |
| `strategy.limits.stop_when_older_than` | 否 | 早于这个时间就不再看 |

**出参**

```json
{ "done_reason": "time_progressed",
  "candidates": [ { "key": "…", "post_id": "…", "media_id": "…", "kind": "video",
                    "url": "https://…", "ext": "mp4", "size_hint": null,
                    "created_at": "2026-09-25T01:33:31Z", "screen_name": "tesla",
                    "day": "2026-09-25" } ],
  "pages": 3, "raw_items": 60,
  "dropped": { "dropped_duplicate": 4, "dropped_by_date": 12, "dropped_by_type": 3,
               "dropped_no_media": 20, "dropped_name": 0 },
  "next_cursor": "…", "seq": 12, "events": [ … ] }
```

| 字段 | 说明 |
|---|---|
| `done_reason` | **为什么停的**（见下表）。必读 |
| `candidates[]` | 候选媒体（形状见 §5.7） |
| `pages` | 翻了几页（含空页） |
| `raw_items` | 服务端给的原始条目数（**到底判据只看它**，不看筛选后的） |
| `dropped` | 各类丢弃计数，排查"这次为什么没东西"用 |
| `next_cursor` | 下次从哪儿继续（`exhausted` 时不出现） |
| `seq` / `events` | 本轮爬取事件（形状同下载事件，schema 里未逐字段约束） |

| `done_reason` | 含义 |
|---|---|
| `exhausted` | 服务端没有更多了 |
| `time_progressed` | 客户端筛选的时间轴已推进到 `since` 之前（**主判据**） |
| `empty_pages` | 连续空页到上限 |
| `cursor_stuck` | 服务端回吐同一个游标（防原地空转刷爆配额） |
| `wanted_collected` | `wanted_keys` 收齐 |
| `page_limit_reached` | 触到 `max_pages`（**不是**服务端到底） |
| `cancelled` / `error` | 取消 / 出错 |

- **`size_hint` 恒为 `null`**：GraphQL 的 media 对象里没有字节数，而爬取阶段逐个探测会白白多出
  N 次请求。真实大小由下载队列在下载前探测；
- 组件**只管筛出候选**，不下不命名；外壳拿到候选后自己决定，再调 `dl.enqueue`
  （两个组件不直接对接）；
- 当前是"跑到停为止再返回"（受 `max_pages` 约束）。长爬取请用小页数反复调用，
  用返回的 `next_cursor` 续爬。

---

## 5. 数据形状

出现在出参里的所有对象都在这一节。**schema 里全部是 `additionalProperties: false`**——
所以你拿到的字段不会比这里多，也不会比这里少。

### 5.1 `user` / `postAuthor`

```json
{ "id": "13298072", "screen_name": "Tesla", "name": "Tesla",
  "avatar": "https://pbs.twimg.com/…bigger.jpg",
  "media_count": 2041, "register_time": "2008-02-10T01:12:32Z" }
```

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | string | 数字 id 的**字符串**形式（超出 f64 精度，不许当数字传） |
| `screen_name` / `name` | string | |
| `avatar` | string | **已归一化**（`https:` + `_bigger`）。外壳不必再替换尺寸 |
| `media_count` | integer \| null | 仅 `user` 有。`null` = 没给这个字段，`0` = 确实没有媒体 |
| `register_time` | string \| null | 仅 `user` 有。RFC3339 UTC |

`postAuthor` 是**子集**：只有 `id` / `screen_name` / `name` / `avatar`。

### 5.2 `media` / `mediaVariant`

```json
{ "kind": "video", "id": "1234567890",
  "url": "https://video.twimg.com/…/x.mp4",
  "poster_url": "https://pbs.twimg.com/…/x.jpg",
  "ext": "mp4", "width": 1920, "height": 1080, "duration_ms": 61500,
  "aspect_ratio": [16, 9],
  "variants": [ { "url": "https://…/x.mp4", "content_type": "video/mp4", "bitrate": 10368000 } ] }
```

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `kind` | string | 是 | `photo` \| `video` \| `animated_gif`（沿用 X 自己的命名） |
| `id` | string | 是 | X 的 media id。比 URL 稳定，适合去重与做文件名 |
| `url` | string | 是 | **可直接下载的地址**。图片是不含查询串的 `media_url_https`；视频/动图已选好最高码率变体 |
| `poster_url` | string \| null | 否 | **封面图**（X 的 `media_url_https`）。见下面两条 ⚠️ |
| `ext` | string | 是 | 扩展名（不含点），由 URL 推断 |
| `width` / `height` / `duration_ms` | integer \| null | 否 | |
| `aspect_ratio` | `[int,int]` \| null | 否 | |
| `variants` | `mediaVariant[]` | 否 | 全部码率变体（**已过滤 HLS**——播放列表不是可下载文件）。图片时不出现 |

> ⚠️ **`poster_url` 与 `url` 是两个不同的东西，别混。**
> `poster_url` 是**封面**：视频/动图是播放前的静帧，图片则是它本身。界面用它画格子、做封面
> （**当图片解码**）。`url` 是**可下载/可播放的地址**（视频是 mp4）。
> 把 mp4 当图片解码的后果是**视频格子整片空白**（这是真实发生过的线上故障）。
> 早于 1.2.0 的契约没有 `poster_url`，那时只能退化成"图片用 `url`、视频留空"。

需要缩略图 / 原图就自己拼查询串：`?name=small`（680px）/ `?name=orig`（原图）。
**裸 URL 等价于 `?name=medium`**。

### 5.3 `post`

```json
{ "id": "123…",
  "created_at": "2026-09-25T01:33:31Z",
  "full_text": "…",
  "lang": "en",
  "views": 4321,
  "favorite_count": 10, "retweet_count": 3, "reply_count": 1,
  "bookmark_count": 7, "quote_count": 2,
  "possibly_sensitive": false,
  "favorited": true, "retweeted": false, "bookmarked": true,
  "medias": [ … ],
  "author": { "id": "…", "screen_name": "…", "name": "…", "avatar": "…" },
  "tags": ["rust"],
  "quoted_id": "…",
  "in_reply_to_screen_name": "…", "in_reply_to_id": "…",
  "retweeted_by": { … } }
```

**必出现**（`required`）：`id` `full_text` `favorite_count` `retweet_count` `reply_count`
`possibly_sensitive` `author` `favorited` `retweeted` `bookmarked`。

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | string | 字符串形式的数字 id |
| `created_at` | string \| null | RFC3339 **UTC**；X 没给时间时为 `null`（**该条目仍会保留**，不会因为解析不到时间被丢掉） |
| `full_text` | string | **已清洗**：长推文用 `note_tweet` 的完整正文（不是被截断的 `legacy.full_text`）；媒体自己的 t.co 占位链接已去掉；短链已换成 `expanded_url` |
| `lang` / `views` / `bookmark_count` / `quote_count` | \| null | 缺失为 `null` |
| `favorite_count` / `retweet_count` / `reply_count` | integer | ≥ 0 |
| `possibly_sensitive` | boolean | |
| `favorited` / `retweeted` / `bookmarked` | boolean | **我的状态**（按钮实心/空心），不是计数 |
| `medias` | `media[]` | **无媒体时该键不出现**（不是空数组） |
| `author` | `postAuthor` | 若原推被转推，这里是**原推作者** |
| `tags` | string[] | 话题标签文本（不含 `#`） |
| `quoted_id` | string \| null | 被引用推文的 id。**契约只给 id，没有内嵌正文**（见 §8） |
| `in_reply_to_screen_name` | string \| null | 本条回复的是谁——是**被回复者**，不是本条作者 |
| `in_reply_to_id` | string \| null | |
| `retweeted_by` | `postAuthor` \| null | 转推时是谁转的；仅当 `include_retweets: true` 时出现 |

### 5.4 分页对象

```json
{ "items": [ … ], "cursor": "…", "end": false }
```

| 字段 | 类型 | 说明 |
|---|---|---|
| `items` | `post[]` 或 `user[]` | 见对应 method |
| `cursor` | string | 下一页要传的游标。**该键不出现 = 服务端没有更多了**（`end: true`） |
| `end` | boolean | `cursor` 不出现时为 `true`。**只由服务端游标决定**，客户端筛选导致的空页不算到底 |

`page<post>` 与 `page<user>` **同形**（都用 `items`），外壳只需写一个翻页循环。

### 5.5 `tweetDetail` 与 `reply`

```json
{ "focal": { …post… },
  "replies": [ { "…post 的全部字段…": "…", "parent_id": "123…", "is_partial_parent": false } ] }
```

`reply` = **`post` 的全部字段**（摊平在顶层）+ `parent_id` + `is_partial_parent`（两个都必出现）。

- `parent_id`：本回复在回复谁（父推文 id）；`null` 表示父推文不在本页；
- `is_partial_parent: true`：父推文不在本页（孤儿）。**这类条目不会被丢弃**。

回复树是**扁平列表**：外壳自己用 `parent_id` 建树（或按返回顺序渲染）。

### 5.6 下载任务与事件（`dl.*`）

**`jobSnapshot`**

```json
{ "job_id": "post-1-media-2", "state": "active", "done": 102400, "total": 3122633,
  "reason": "…", "error": { … }, "tag": "123", "dest_path": "/Users/me/dl/…/x.mp4" }
```

| 字段 | 类型 | 必出现 | 说明 |
|---|---|---|---|
| `job_id` | string | 是 | 外壳生成的幂等键 |
| `state` | string | 是 | `waiting` \| `active` \| `paused` \| `error` \| `complete` |
| `done` / `total` | integer | 是 | 已下 / 总字节数（未知时为 0） |
| `reason` | string | 否 | 结束原因短标签：`cancelled` / `not_found` / `integrity_failed` / `truncated` / `transport` / `disk_full` / `upstream` / `invalid` … **按它判断，不要按文案** |
| `error` | `error` | 否 | 结构化错误（见 §6） |
| `tag` | string | 否 | 外壳放的不透明标记 |
| `dest_path` | string | 否 | 目标路径 |

> **没有单独的 `cancelled` 状态**：取消表现为 `error` + `error.code == "cancelled"`。
> 少一个状态，外壳少一条分支。

**`downloadEvent`**（四种，用 `kind` 判别）

```json
{ "kind": "progress",  "job_id": "…", "done": 1024, "total": 204800 }
{ "kind": "completed", "job_id": "…", "path": "…", "bytes": 204800,
  "integrity": { "result": "verified", "expected": 204800, "actual": 204800 } }
{ "kind": "failed",    "job_id": "…", "reason": "not_found", "error": { … } }
{ "kind": "skipped",   "job_id": "…", "reason": "already_present" }
```

- `progress.done` **单调不减**（续传时从断点开始）；
- `integrity` **只有两种形状**：
  `{ "result": "verified", "expected": N, "actual": N }`（服务端给过大小）
  与 `{ "result": "unverified", "actual": N }`（没给过，**不能声称"校验通过"**）。

### 5.7 `candidate`（爬取产出）

```json
{ "key": "…", "post_id": "…", "media_id": "…", "kind": "video",
  "url": "https://…", "ext": "mp4", "size_hint": null,
  "created_at": "2026-09-25T01:33:31Z", "screen_name": "tesla", "day": "2026-09-25" }
```

**必出现**：`key` `post_id` `media_id` `kind` `url` `ext`。
`key` 是去重键（默认媒体 id）；`day` 是推文所在日期（`YYYY-MM-DD`，**UTC**），
按天分目录时用它。`size_hint` 目前恒为 `null`（见 §4.5）。

---

## 6. 错误：只按 `code` 判断

```json
{ "error": { "code": "rate_limited", "message": "…", "retry_after_s": 120,
             "endpoint": "user_medias", "status": 429, "context": null,
             "detail": { "kind": "timeout" } } }
```

| `code` | 何时出现 | 外壳该做什么 |
|---|---|---|
| `invalid_request` | 未知 method、缺字段、字段类型错、参数不合法 | **别重试**，是调用方的问题（拼错了字段/类型） |
| `unauthorized` | 凭据失效 / 被登出 / 未注入凭据 | 提示重新登录，**别重试** |
| `rate_limited` | 触发限流，带 `retry_after_s` | 展示倒计时；组件内部已挂起，别催 |
| `not_found` | 用户 / 推文 / 任务 / 媒体不存在 | 展示"不存在"，别重试 |
| `upstream` | 上游 HTTP 错误，带 `status` | 5xx 可退避重试；4xx 先怀疑调用方式 |
| `parse` | 解析失败 = **X 可能改版了** | **这是最该报警的一类**；带上 `context` 报给组件作者 |
| `transport` | 网络 / 代理 / DNS，`detail.kind` 说明类别 | 提示网络问题；组件内部已重试过 |
| `cancelled` | 调用方取消 | 正常路径，不是错误 |
| `internal` | 组件的 bug | 报出来 |

**字段出现时机**

| 字段 | 类型 | 出现时机 |
|---|---|---|
| `code` | string enum | 总是 |
| `message` | string | 总是。**给人看，不要用于判断** |
| `retry_after_s` | integer | 仅 `rate_limited` |
| `status` | integer | 仅 `upstream`（上游 HTTP 状态码） |
| `context` | string | 仅 `parse`（解析失败的位置） |
| `endpoint` | string | 逻辑端点名（如 `user_by_screen_name`）。**不是 URL 路径** |
| `detail.kind` | string | 仅 `transport`：`timeout` \| `connect` \| `body` \| `fixture` \| `other` |

三条纪律：

1. **绝不匹配 `message` 文案**——它是给人看的，改版即变。
   反面教材：曾有实现用匹配引擎输出的文案（`"exit 3"` / `"404"`）判断能否重试，引擎一换就全错；
2. **`parse` 要报警**（这是"X 改版了"的信号，不是"网络抖了"）；
3. **只有 `transport` 值得外壳自己再兜一层重试**：组件内部对单次请求已重试 3 次
   （150ms / 450ms 退避），但实测代理会出现**持续一两秒的整段拒连**。
   外壳用 1s / 3s 这样的退避再试一次是合理的——**别把 `unauthorized` 也重试成三倍慢的
   `unauthorized`**。

---

## 7. 一份可以直接抄的调用时序

### 7.1 首次启动

```
1. 起 sidecar（--port 0，带 XSPIDER_STATE_DIR / XSPIDER_ARIA2_PATH）
2. 读 stdout 的 ready 行 → 拿到 port / token / version
3. system.version    → 主版本不匹配就拒绝启动（给可操作提示）
4. system.methods    → 打一条日志；缺依赖的 method 就明确报错
5. auth.set_cookie   → cookie 由外壳从 Keychain / WebView 拿，只进不出
6. net.set_proxy     → 代理变了就再调一次（本机代理端口一天会变好几次）
7. net.set_limits    → 一次（api_rps / api_burst / cdn_concurrency / cooldown_s）
```

### 7.2 取一页并下载

```
8.  auth.whoami              → 校验登录、拿当前账号
9.  fetch.get_user           → 拿 user_id
10. fetch.user_medias        → 一页 + 游标（要不要继续翻页由外壳决定）
11. net.probe_size × N       → 决定下不下、要不要多连接
                               （探测失败按"未知"继续，**别当致命错误**）
12. dl.enqueue × N           → job_id 自己生成（建议 "<post_id>-<media_id>"）
13. dl.events(since) 轮询    → 画进度（节流自己做）
    dl.list()                → 判断"结束了没有"（权威）
14. 收尾：dl.prune / system.shutdown
```

### 7.3 暂停与恢复（**最容易做错的一步**）

```
dl.pause { job_id }        → 任务停在 paused，断点保留
...
dl.resume { job_id }       → 从断点继续。**不要再 dl.enqueue 一次**：
                             同一个 job_id 只会得到 already_known，任务原地不动。
```

失败重试同理：`error` 状态的任务用 `dl.resume` 重来（`dl.resume` 对 `error` 也有效），
不是重新入队。

### 7.4 写操作

```
fetch.mutate { action: "favorite", tweet_id: "…" } → { ok: true }
// 失败会原样上报（不会谎报成功）；按 error.code 判断
// 关注类动作成功后，外壳侧自己的关注态缓存也要失效
```

### 7.5 爬取

```
crawl.run { source: "medias", user_id, strategy: { since, until, limits: { max_pages: 5 } } }
  → 看 done_reason 决定文案与"下次是否续爬"
  → 外壳在 candidates 上做产品语义的过滤（排除清单、全选等）
  → 决定下不下的，再 dl.enqueue
  → next_cursor 非空就用它续爬
```

---

## 8. 已知边界（用之前先知道）

| 边界 | 说明 |
|---|---|
| **没有推送式事件流** | JSON-RPC 与 C ABI 都不支持流，所以统一用"带游标的增量轮询"（`dl.events`）。进程内形态另有 `subscribe()`。见 ADR-029 |
| **没有 `quoted_post` 内嵌正文** | `post` 只给 `quoted_id`。外壳若要渲染引用推文的正文，需另调 `fetch.tweet_detail`。见 `docs/06` §5 |
| **`crawl.run` 是"跑到停为止"** | 受 `max_pages` 约束；长爬取请用小页数反复调用 |
| **`crawl` 的 `size_hint` 恒为 `null`** | GraphQL 不提供字节数；真实大小由下载队列在下载前探测 |
| **`net.set_limits.cdn_concurrency` 只在队列创建时生效** | 信号量不能缩容。要"运行中调并发"需要另外的接口 |
| **`dl.plan` / `dl.report` 尚未实现** | `host` 逃生舱（iOS 后台 `URLSession` 一类平台强约束），已登记形状，调用会得到 `invalid_request` |
| **只有 Aria2Next，不支持上游 aria2** | 选项集与行为不同，见 [`NOTICE`](../NOTICE) |
| **`crawl.run` 的事件形状未逐字段约束** | schema 里 `events` 是通用对象数组；`dl.events` 的事件是逐字段约束的 |
| **完整性只管"字节对不对"** | "这个文件真的是图片/mp4 吗"（魔数、HTML 误页）由外壳负责——分工见 `docs/06` §5.4 |
