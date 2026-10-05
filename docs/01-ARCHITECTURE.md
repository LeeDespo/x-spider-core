# 01 · 架构：边界、契约与引擎

> 本文回答三个问题：**组件切在哪**、**契约长什么样**、**下载的字节谁搬**。
> 所有结论都对应到具体实现细节，不是风格偏好。

---

## 1. 为什么是「两个组件 + 一个内核」，而不是一个大组件

取数与下载的依赖、配额、生命周期、测试面全都不一样：

| | 取数（fetch） | 下载（download） |
|---|---|---|
| 认证 | 需要 cookie + `x-client-transaction-id` 签名 | 媒体 CDN 无需认证 |
| 配额 | 受账号配额约束，要令牌桶 + 429 熔断 | 另一套 CDN 限流，与 API 配额独立 |
| 生命周期 | actor 内串行短请求 | 长任务 + 常驻子进程 + 断点续存 |
| 测试 | 可录制回放（纯数据） | 需要真字节 + 本地 HTTP 服务 |
| 变更来源 | X 改版（queryId/features/字段） | aria2 / 网络栈 / CDN 策略 |

**合并后两者会互相拖累**（一边限流把另一边冻住），且唯一共享物只是"媒体 URL"这一个字符串——
把它做成 DTO 字段就够了。组件 A 与组件 B **不直接依赖**，衔接见 §4。

内部共享内核 `xspider-core` 承担：HTTP 客户端、凭据注入、限流闸门与熔断、请求签名、
错误分类、DTO 序列化。**不抽这层，限流就会被写两遍，而 429 是这条链上最容易翻车的地方。**

---

## 2. 契约：一个入口 + JSON 边界

```
xspider_version() -> char*                   // "1.5.1"，握手用
xspider_call(method, json_in) -> json_out    // 所有能力
xspider_free(char*)
```

**为什么是 JSON 边界而不是生成的强类型绑定**：

1. **语言中立**：Swift / Kotlin / C# / Python / 甚至 shell + curl 都能调，不必为每种语言生成绑定；
2. **换组件最容易**：只要 method 名与 JSON 字段不变，替换二进制就完成升级；
3. **可离线测**：契约测试就是"给定 json_in，断言 json_out"，不依赖真网络；
4. **不被生成器绑架**：绑定生成器只是**可选胶水**（见 §7），不参与契约定义。

**代价（要认）**：序列化开销、类型安全靠 JSON Schema + 契约测试维持、错误在运行期才暴露。
对策：`contract/xspider.schema.json` 做机器可读契约 + 契约测试覆盖每个 method 的正反例。

**契约里绝对不许出现**：queryId、`features` 常量、端点路径、HTTP 头、Rust 类型名。
一旦进去，契约就被焊死在今天的 X 上——而做这件事的全部意义就是不要被焊死。

---

## 3. 取数契约（fetch）

```
auth.set_cookie       {cookie, csrf?}                        -> {ok}
auth.whoami           {}                                     -> {account}
net.set_limits        {api_rps, api_burst, cdn_concurrency, cooldown_s} -> {ok}
net.set_proxy         {url}                                  -> {ok}
net.status            {}                                     -> {state, rate_limited_until?}
net.probe_size        {url}                                  -> {size}
fetch.get_user        {screen_name}                          -> {user}
fetch.user_medias     {user_id, cursor?, count?}             -> {items[], cursor?, end:bool}
fetch.user_tweets     {user_id, cursor?, count?, require_media?, include_retweets?} -> {items[], cursor?}
fetch.tweet_detail    {id}                                   -> {focal, replies[], cursor?}
fetch.search_timeline {screen_name, since, until, media_only?, cursor?} -> {items[], cursor?}
fetch.home_timeline   {mode, cursor?}                        -> {items[], cursor?}
fetch.following       {user_id, cursor?, count?}             -> {items[], cursor?}
fetch.is_following    {screen_name}                          -> {following}
fetch.mutate          {action, tweet_id?, screen_name?}      -> {ok}
```

（完整清单、字段类型与默认值见 [`07-API-REFERENCE.md`](07-API-REFERENCE.md)；
`fetch.mutate` 的 `action` ∈ `favorite` / `unfavorite` / `retweet` / `unretweet` /
`bookmark` / `unbookmark` / `follow` / `unfollow`——**推文类动作带 `tweet_id`，关注类带
`screen_name`**，不是笼统的一个 `id`。）

### 三条硬性设计
1. **给「单页 + 游标」，不给自动翻页的流**。翻页时机、过滤、去重是业务语义，
   必须让调用方看得见游标（见 `02-X-DOMAIN-NOTES.md` 的 A1、B3、B4）。
2. **错误必须结构化**（见 §6），不许靠文案匹配。
3. **取消要能从外壳传播进 Rust**，真正中止请求与下载。

---

## 4. 爬取与下载的衔接：「候选清单 + 策略参数」

**爬虫的难点不在下载字节，在策略**：终止判据、去重、筛选、跳过。这些一半是业务，
一半要读**本地文件系统**（跨端路径与权限语义不同）。切法如下：

| 关注点 | 归属 | 理由 |
|---|---|---|
| 取页、游标推进、到底信号 | **组件** | X 语义，易变面 |
| 终止判据族（到底 / 时间轴推进 / 连续空页 / 游标未推进 / 收齐 wanted） | **组件实现，参数化** | 踩过最多坑的地方，必须一次写对 |
| 页间节流、限流挂起与恢复 | **组件** | 节奏必须唯一所有者，否则 429 风暴 |
| 日期/媒体类型过滤边界 | **组件实现，参数化** | 规则本身是知识（无 `createdAt` 放行等） |
| 跨页 URL 去重、重复转推去重 | **组件** | 响应层面的问题 |
| 目录、文件名 | **外壳算好再传** `dest_dir` + `file_name` | 用户偏好 + 本地路径；模板引擎可放内核当纯函数 |
| 「已下载过」的判定开关 | **外壳设置项** | 产品语义 |
| 记录文件的读写与格式 | **组件**（路径由外壳给） | 格式必须唯一，否则三端互相看不懂（静默错误） |
| 选择集（全选/反选/排除法） | **外壳** | 产品语义，组件不需要理解"全选=全部" |
| 视口填充式无限滚动 | **外壳** | "填满两屏就停"是渲染问题，用单页原语即可，不走 crawl |

**衔接方式：组件产出候选，外壳决定要不要下、叫什么名、放哪儿，再调下载组件入队。**

```
crawl.run {
  source: "medias" | "tweets", user_id, cursor?,
  strategy: { since?, until?, media_types?, wanted_keys?,
              limits: { page_size?, page_throttle_ms?, max_pages?,
                        empty_page_limit?, stop_when_older_than? } }
} -> { done_reason,
       candidates: [ { key, post_id, media_id, kind, url, ext, size_hint,
                       created_at?, screen_name, day } ],
       posts: [ /* 本轮保留的完整 post DTO，与 candidates[] 对应 */ ],
       pages, raw_items, dropped, next_cursor?, seq, events }

done_reason: exhausted | time_progressed | empty_pages | cursor_stuck
           | wanted_collected | page_limit_reached | cancelled | error
```

`time_progressed` 由 `since` 与 `limits.stop_when_older_than` 两个时间边界触发：前者表示时间轴已
越过请求下限，后者表示已遇到早于该阈值的推文。两者都表示本轮到达时间边界；需要区分业务原因时，
调用方应结合自己提交的策略参数判断。

契约 1.5.0 起，结果同时包含有损的 `candidates[]` 与同一批数据的完整 `posts[]`。
外壳可用 `post_id` 将二者关联；候选提供媒体下载信息，`posts[]` 保留命名和记账需要的推文 DTO。

> 实现把 §4 的「按页批量给候选」落成了**一次调用返回整轮结果**（受 `max_pages` 约束）：
> 长爬取由外壳用小页数反复调用、用返回的 `next_cursor` 续爬。这样进度对调用方可见，
> 也不必把长任务塞进一次请求里。下载事件也通过带游标的增量轮询读取，三种形态都没有契约级
> 推送流（见 ADR-029）。

四个要点：
1. **`done_reason` 必须显式**——"翻到服务端尽头"与"被策略提前终止"是两种语义，
   外壳靠它决定文案与下次是否续爬。
2. **`wanted_keys` 进契约，`excluded_keys` 不进**：前者改变翻页终止条件（收齐即停 = 省请求，成本相关）；
   后者只是产品语义，外壳在候选清单上过滤即可，**零额外请求**。
3. **按页批量给候选，不要逐条回调外壳**——跨边界次数与 async 复杂度都不可控。
4. **限流状态必须整体归组件**（含"网络异常 TTL 到期后放行"这种细节）。若外壳还要告诉组件
   "现在能不能发请求"，限流策略就被劈成两半。

> **`size_hint` 的实现现状（实测补充）**：GraphQL 的 media 对象里**没有**字节数
> （只有宽高与视频的时长/码率），而"码率 × 时长"估出来会**差 5 倍**
> （实测 76 MiB vs 真实 14.48 MiB，见 `docs/02` §E9）。
> 所以爬取阶段**不做**逐个探测（一页 20 个媒体就是白白多 20 次请求），
> `size_hint` 缺省时不返回该字段；**真实大小由下载队列在下载前探测**
> （`expect_size` 缺省时自动 `HEAD`，失败退回 1 字节 `Range`），
> 于是完整性校验照样成立，而且引擎选择拿得到真值。

---

## 5. 下载：任务归属与实现选项

### 5.1 任务归组件所有

```
dl.enqueue { job_id, url, dest_dir, file_name, expect_size?,
             requirements: { resume: bool, segments: u8 } }   -> { accepted_by }
dl.pause | resume | cancel { job_id }                          -> { ok }
dl.status { job_id }                                           -> { state, done, total, error? }
dl.list   {}                                                   -> { jobs[] }
dl.events {since}                                              -> {seq, events[]}（带游标的增量轮询）
```

| 东西 | 归属 | 理由 |
|---|---|---|
| 队列、并发、调度 | **组件** | 并发必须唯一所有者，否则 CDN 限流降级失效 |
| 任务状态机（waiting/active/paused/error/complete） | **组件** | 跨端一致 |
| 字节搬运、断点续传、重试退避、完整性校验 | **组件** | 平台无关、最易写错 |
| **`job_id`** | **外壳生成并传入，组件幂等** | 外壳要用它把进度映射回"哪条推文的哪张图"；同 id 重复入队只算一次 —— 一次解决「跨页重复投递 / 暂停后重试 / 重启对账」三件事 |
| 领域对象关联（post/media） | **外壳**（组件只存不透明 `tag`） | 组件不可能知道 TwitterPost 是什么类型 |
| 下载记录的**写** | **组件**（完成事件的产生者） | 两个写者必然出现"文件下好了但记录没写"的静默不一致 |
| 通知、休眠断言、进度条 | **外壳** | 平台服务；组件只发事件 |
| 目录与文件名 | 外壳算好再传 | 见 §4 |

**两处持久化按 `job_id` join**：组件存自己的任务状态（供崩溃恢复续传），
外壳存 `job_id → post/media` 投影（供 UI）。重启后外壳用 `dl.list()` 与自己的记录对账。

### 5.2 下载实现选项（契约与实现状态分开看）

| 后端 | 谁搬字节 | 用途 |
|---|---|---|
| **`http`（内置，reqwest + Range 分片 + 续传）** | 组件自己 | **保底**：零外部依赖、跨端行为一致、离线测试的载体（本地 HTTP server 做 fixture） |
| **`aria2`（外派进程 + JSON-RPC）** | aria2-next 进程 | 性能路径：多连接分片、成熟续传与重试 |
| `host`（早期架构标签） | — | **未设计、未实现**。`dl.plan` / `dl.report` 不是当前契约 method，也没有请求/响应形状；若有真实平台需求，再另行定义 ADR 与契约。 |

当前有两个已实现的下载后端：`http` 与 `aria2`。`host` 一词只保留为早期架构讨论的标签，
不能据此推断存在外壳执行计划、回报 method 或已登记的接口形状。

三条纪律：
1. **引擎选择策略留在组件内部**（按大小/可用性/设置决定），不暴露给外壳；
2. **不把 aria2 专有选项**（`stream-max-connections`、`file-allocation` 之类）暴露进契约，
   否则"换引擎"又从契约漏出去了；
3. **`expect_size` + 完整性校验由组件负责**，与后端无关；完成事件要能区分
   `completed | integrity_failed | skipped | failed{reason}`。

**两个后端必须行为一致**，这不是"尽量"而是验收条件。实测过的两条容易漏的：

- **代理要逐任务显式传给 aria2**：子进程不认识 `HTTPS_PROXY`，
  而内置后端的 reqwest 会自动读环境变量——不显式传，需要代理的机器上就只有内置后端能工作；
- **`requirements.segments > 1` 在 Aria2Next 里对应的选项名是 `stream-max-connections`**：
  上游 aria2 的 `--split` / `--max-connection-per-server` 在这个 fork 里已退役，照抄旧名字等于没设。

**"运行中换代理"对下载同样成立**：`net.set_proxy` 的承诺是"不必重启进程"，
所以下载器不能在构造时把代理焊死（`docs/DECISIONS.md` ADR-035）。

---

## 6. 错误分类（结构化，不可省）

```json
{ "error": { "code": "rate_limited", "message": "...", "retry_after_s": 120,
             "endpoint": "user_medias", "detail": {...} } }
```

```
unauthorized          凭据失效/被登出            → 外壳应提示重新登录，不要重试
rate_limited          限流（带 retry_after）     → 组件内部退避/挂起，外壳展示
not_found             用户/推文不存在
upstream{status}      服务端错误（含 404 与 queryId 失效的区别，见 `02-X-DOMAIN-NOTES.md` A2）
parse{context}        解析失败 = X 可能改版了     → 这是最需要报警的一类
transport{...}        网络/代理/DNS
cancelled             调用方取消
invalid_request       参数无效或当前未实现的 method
internal              组件内部错误
```

**为什么必须结构化**：一个真实的反面教材——下载侧曾用**匹配引擎输出的错误文案字符串**
（`"exit 3"` / `"404"`）来判断能否重试，引擎一换就全错。别复制这种脆弱性。

---

## 7. 绑定生成器（BoltFFI / UniFFI）的定位

**它们是可选胶水，不是契约。** 事实（2026-09 核查）：

| | BoltFFI | UniFFI |
|---|---|---|
| 版本 | 0.31.0（0.x） | 0.32.2（0.x，但 Firefox 生产使用） |
| 历史 | 约 7 个月 | 多年 |
| 目标语言 | Swift/Kotlin/Java/C#/TS/Python | Kotlin/Swift/Python |
| 维护集中度 | 单人占约 62% 提交 | Mozilla 主导 |
| 生产案例 | 无具名案例（自报性能数字） | Firefox mobile + desktop |

**结论**：可以用，但要 (1) 锁定精确版本；(2) 只在**外壳侧**使用生成绑定，
ABI 层只允许加法式变更；(3) 不把生成器的类型放进契约。
**不建议把核心押在 BoltFFI 上**——它的破坏性更新会直接变成你的维护负担。

---

## 8. 部署形态：sidecar 优先

| 形态 | 换组件的方式 | 平台风险 |
|---|---|---|
| **sidecar 可执行文件**（主） | 换一个二进制 | 几乎无（各自 ad-hoc 签名即可，互不验证） |
| **cdylib**（次） | 换一个文件 | 见 `03-FFI-SIGNING-PACKAGING.md`：一旦启用 hardened runtime，ad-hoc 产物加载任何 dylib 都会被拒（实测） |

**合并的是部署，不是设计**：两个组件可以在**同一个 sidecar 进程**里链入，
这样"爬一页 → 过滤 → 建任务"的粘合可以在进程内用 Rust 写，外壳只订一次事件流；
对外仍是两个独立组件（能单独升级）。

---

## 9. 版本与兼容

- 契约版本用语义化版本，`xspider_version()` 返回；外壳启动时握手，不匹配**拒绝启动并给明确提示**，
  不要降级到"部分功能可用"。
- 规则：**method 与字段只增不改不删**；要改语义就新增 method 名并让旧名保留一个过渡期。
- 每次契约变更同步更新 `contract/xspider.schema.json` 与契约测试的正反例。
