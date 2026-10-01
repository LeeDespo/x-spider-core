# 02 · X 领域知识（**不要重踩**）

> 这一节的内容全部来自一个已经能运行的 macOS 移植项目（源码见仓库 `x-spider-mac`）
> 与其上游（Tauri + React 的 `MiningCattiva/x-spider`）的实测结论。
> 每一条都是**真金白银的返工**换来的，照着做能省几周。

---

## 0. 移植纪律：与上游逐字对齐

X 的 GraphQL 端点对 `queryId` / `features` / `variables` 的格式极其敏感，分页语义也有坑。

**权威参照顺序**：
1. `x-spider-mac/src/twitter/api.ts`（上游 TypeScript 原版，GPL-3.0）——**行为权威**；
2. `x-spider-mac/XSpiderMac/Sources/XSpiderMac/Services/TwitterAPI.swift`——Swift 实现 + 大量实测注释；
3. `x-spider-mac/AGENTS.md` 与 `docs/DEVELOPMENT.md`——取舍与教训。

**凡是要动请求构造或分页逻辑，先读上游对应函数，逐字对齐，再动手。**
不要"顺手优化"请求格式——你以为的优化通常是 404 或空页的来源。

---

## A. 请求构造

**A1. `variables.cursor` 不能硬编码成 `null`。**
首页请求必须**省略** cursor 键（上游用 `JSON.stringify` 天然丢弃 `undefined`），
翻页才传真实游标。传 `null` 会让每一页都请求第一页 → 无限加载 / 重复检索同一页。
Rust 侧对应：`Option<String>` 序列化时用 `skip_serializing_if = "Option::is_none"`。

**A2. 搜索端点必须 POST + JSON body。**
GET 一律 404。**且这个 404 与 queryId 无关**——实测新旧两个 queryId 用 POST 都返回 200，
只有随机乱写的才 404。别再把 404 误判成"queryId 失效"去折腾自愈。

**A3. queryId 要有自愈能力。**
从 X 的网页/JS 里提取当前 queryId。**提取正则必须锚定 `operationName:"<目标端点名>"`**，
否则会命中排在前的同名变体（Bookmark/List 等）。

**A4. `x-client-transaction-id` 需要按 X 的算法生成。**
涉及首页抓取 + 贝塞尔曲线动画状态推导。这是最容易被忽略、也最容易因改版失效的一块：
**单独成模块、单独测**，不要在请求组装里内联。参考
`x-spider-mac/.../Services/XClientTransaction.swift`（约 490 行）。

**A5. headers 要与上游一致**：user-agent、`x-csrf-token`（= cookie 里的 `ct0`）、
`x-twitter-auth-type`、`x-twitter-active-user` 等。少一个就可能 403 或返回空数据。

**A6. `features` 常量随 X 改版变化**，且每个端点的 features 组合不同。
它们**属于组件内部实现**，绝不进契约。

---

## B. 限流与节奏（**做错就会 429 风暴**）

**B1. 页间节流 400–500 ms。** 上游靠浏览器渲染节奏天然限速，Rust 循环没有，必须显式等价。

**B2. 绝不要把"翻到服务端尽头"当成无限滚动的实现方式。**
上游的无限滚动只补拉到"视口填满（约两屏）"，剩余靠用户滚动逐页触发。
无停止条件的连发循环 = 429 风暴。

**B3. 爬虫里 `fetch` 返回后第一件事就是推进游标**，过滤与日期判断都在推进之后。
否则被过滤清空的页会被无限重抓。

**B4. 空页必须终结**：解析出 0 条时返回 `cursor: null`（到底信号）。

**B5. 限流时挂起，不要失败退出。**
cursor 与进度保留，等状态恢复自动续跑——避免"越限越试"把限流拖长。
注意细节：网络类异常要有 TTL，到期后放行让真实请求重新判定，
否则"断网时无成功请求 → 状态永不清除 → 无限挂起"（用户实测反馈的
"代理恢复后应用仍卡很久，除非重启"）。

**B6. GraphQL API 与媒体 CDN 是两个域、两套配额、必须分别治理**：
`x.com/i/api`（受账号配额，管翻页/爬虫）vs `pbs.twimg.com` / `video.twimg.com`（管图片视频下载）。

**B7. 429 要有熔断 + 冷却**（触发后暂停该类端点），而不是继续重试。

**B8. 游标不推进要当作到底。**
X 偶发回吐与上一页相同的 cursor（限流/游标失效）。此时后续页必然重复，
不判停会原地空转刷爆配额。**不要用页数上限兜底**，用"游标是否推进"。

---

## C. 解析

**C1. 评论区会插广告。**
`conversationthread-*` 里 `itemContent.promotedMetadata` 非空即为广告（实测约 3 条/会话）。
**三个解析入口都要过滤**（module instructions / tweet entries / reply nodes），漏一个就在对应界面露出广告。

**C2. 回复层级不要压平。**
`legacy.in_reply_to_status_id_str` 与 `in_reply_to_screen_name` 是响应里**现成**的。
**父不在本页的孤儿不能丢**——标记为 partial parent。
「回复 @xxx」必须用**被回复者**，不是本条作者。

**C3. 重复转推必须去重。**
一段时间内反复转发同一条推文会让同一 ID 出现多次；在 SwiftUI 里表现为**空白卡片**
（列表 id 重复导致渲染错乱）。Rust 侧同理：ID 重复会让下游去重表/映射错乱。

**C4. 「评论的评论」只有贴主的，是 X 服务端行为**，不是解析 bug
（实测：以评论为 focal 也只返回贴主那条，且无"更多回复"游标）。**别再为此改解析。**

**C5. 媒体在 `legacy.entities.media`，早就有。**
遇到"某功能好像不存在"，先分清**解析缺失**还是**渲染缺失**——历史上多次是后者。

**C6. 图片 URL 的 `?name=` 参数**：`small` 是 680px 宽（不是 120px），`orig` 是原图。
缩略图要按目标尺寸**降采样解码**，别全尺寸解码后缩放（网格滑动会卡）。

**C7. 视频取最高码率的 variant；GIF 用 `video.twimg.com` 的 mp4。**
`video_info.variants` 里要过滤掉 `application/x-mpegURL`。

**C8. 长推文的正文在 `note_tweet.note_text`，`legacy.full_text` 是截断的。**
超过 280 字的推文会带 `note_tweet`，此时 `legacy.full_text` 以 `…` 结尾。
只读 `legacy.full_text` 不会报错——**它只是静默少一半内容**，这是最难发现的一类偏差。
参考实现读的是 `item.note_tweet.note_text ?? legacy.full_text`，照它做。

**C9. 正文里的 t.co 链接要清洗（参考实现的两步）。**
1. 去掉 `entities.media[].url`——它是**那张图自己的**占位链接，媒体已经在 `medias` 里了；
2. 其余 `entities.urls[].url` 换成 `expanded_url`，正文里才是可读链接。
长推文的实体集在自己的 `note_tweet.entity_set` 里（**没有 `entities` 这一层**），
形状与 `legacy.entities` 不同，两处都要过。

**C10. 头像要归一化：`//` → `https://`，`_normal` → `_bigger`。**
参考实现给外壳的就是这个值（`_normal` 是 48px，列表里会糊）。
组件与外壳给同一个值，接入时外壳那两行替换才能删掉。

---

## D. 筛选与终止语义（最容易写错的一类）

**D1. 去重必须先于筛选。** 先 `seen_post_ids`，再按日期/类型过滤。

**D2. 客户端筛选的终止判据必须是"时间轴推进"**（`oldest_seen_at < since`），
**不要用"连续空页计数"作为唯一判据**——账号停更一两个月的空窗期会被误判成"没有内容"
（用户实测反馈）。空页计数只能作为**辅助**上限（移植项目里用 5 页）。

**D3. 日期边界**：
- 传 X 的 `until:` 是**排他**的 → 要传"用户选的结束日 + 1 天"；
- `since`/`until` 用**本地时区**格式化（不是 UTC）；
- UI 的"至"当天必须整天包含（内部表示用 inclusive end）。

**D4. 无 `createdAt` 的条目要放行**（不能因为解析不到时间就丢掉）；纯文字推文不受"媒体类型筛选"影响。

**D5. 判定"到底"只看服务端原始条数**，客户端筛选后的条数不能作为到底依据。

---

## E. 下载侧

**E1. 跳过已下载有两种判定依据**，行为不同：
- **按文件名**：落盘名末尾带资源索引，改名即失效；
- **按记录文件**（如 `.downloaded.json`）：判定不依赖文件名，改文件名也不失效，可加速二次同步。
两者是**用户设置项**，不是实现细节。

**E2. 完整性校验是唯一的"成功"判据**，不是"任务返回成功"。
按 `expect_size` + 类型校验实际落盘文件；对不可达 URL 的实测表现是
**留下 0 字节文件**，且失败退出码不止一种——所以不能只看退出码。

**E3. 引擎切换必须丢弃断点。**
URLSession 的 `resumeData` 与 aria2 的半成品拼在一起会产出损坏文件，
所以"引擎与上次不同 → 丢弃断点重下"。

**E4. aria2 会留下 `.aria2` 控制文件与临时文件**，清理与命名不能被当作"引擎内部细节"
扔给引擎——否则换引擎就漏文件。

**E5. 临时文件与目标目录**：落盘要原子（先写临时文件再 rename），
且不能让引擎直接写目标目录（会与"用户看到半个文件"冲突）。

## F. 与上游的对应关系（移植时按这个顺序读）

| 你要实现的东西 | 先读 |
|---|---|
| UserMedia / UserTweets / TweetDetail 请求与解析 | `src/twitter/api.ts` |
| 搜索页媒体列表加载与翻页 | `src/stores/homepage.ts` 的 `loadPostList` / `loadMorePostList` |
| 创建任务爬虫（下载全部） | `src/stores/download.ts` 的 `runCreationTask` |
| 无限滚动触发节奏 | `src/components/InfiniteScroll.tsx`（**视口填满即停**） |
| 网格渲染 / 缩略图 | `src/components/homepage/PostListGridView.tsx`（`loading="lazy"` + `?name=small`） |

---

## G. 实测响应形态（2026-10-01 采集；M0 + M1）

> 这三条都是**真实抓到的响应**（脱敏后入库：`fixtures/user_by_screen_name/`），
> 不是推断。它们直接决定了错误分类怎么写——**不要凭直觉改**。

**G1. 用户不存在 → HTTP 200 + `{"data":{}}`。**
没有 `user` 键，也**没有 `errors[]`**。上游 `getUser` 的语义同样是"取不到 `legacy` 就是找不到"，
两者一致。所以判据是**结构**：`data` 是对象但里面没有 `user` → `not_found`。

**G2. 未带凭据 → HTTP 403 且响应体为空。**
连一个字符都没有，只有 `x-transaction-id` 头。
**这意味着"从响应体里读失败原因"这条路根本不存在**——分类只能靠状态码。
（也是"不许用错误文案做判断"这条铁律的又一个现场证据。）

**G3. 真实响应里带 `x-rate-limit-limit` / `-remaining` / `-reset`。**
实测一次正常请求：`limit=150, remaining=148`。
→ 可以据此做**主动**限流（在配额耗尽前降速），而不是等 429 才知道。
M1 可以考虑把这三个头接进 `net.status`。

**G4. `data.user.result` 里值得注意的非 `legacy` 字段。**
- `rest_id` 在 `result` 层（不在 `legacy` 里）；
- `__typename` 可能是 `UserUnavailable`（冻结/不存在/被屏蔽）→ 归 `not_found`；
- `legacy_extended_profile.birthdate` 会出现在响应里——**这是个人数据**，
  fixture 脱敏脚本专门处理它（`script/redact_fixtures.py`）。

对应的判据总结（`crates/xspider-fetch/src/user.rs`）：

| 上游表现 | 我们的错误码 | 为什么 |
|---|---|---|
| `data` 是对象但没有 `user` | `not_found` | G1 |
| `__typename == "UserUnavailable"` | `not_found` | 与上游一致 |
| `errors[].code` ∈ {50, 63} | `not_found` | 数字枚举，**不看 message** |
| `result` 在但 `legacy` 缺失 | `parse` | 结构对不上 = 可能是改版，要报警 |
| 既没有 `data` 也没有 `errors` | `parse` | 同上，并在消息里列出顶层键 |
| HTTP 401 / 403 | `unauthorized` | G2 |
| HTTP 429 | `rate_limited`（带冷却） | `docs/02` §B7 |

**G1 的代价要认**：如果 X 哪天把 `user` 这个键改名，会被误判成 `not_found` 而不是 `parse`。
兜底是**正向路径的 canary**（对真实用户断言解析成功）——改名会让它立刻红，不会静默。

---


---

## H. M1 时间线端点的实测发现（**每条都会让整页解析成空，务必先读**）

> 全部来自 2026-10-01 的 live 采集，样本已脱敏入库（`fixtures/`）。
> 这一节的每一条都曾经让"明明有数据"的响应解析成空页——而症状看起来像
> "端点逻辑坏了"，实际是**响应形态与上游代码不一致**。

**H1. 同一个端点的响应里，**两种用户结构并存**——必须都认。**
实测：`user_medias` / `user_tweets` 的作者用老结构
（`legacy.screen_name` / `legacy.profile_image_url_https`），
而 `search_timeline` / `home_timeline` / `following` / `tweet_detail` 用新结构
（`core.screen_name` / `core.name` / `avatar.image_url`，且**可能整个没有 `legacy`**）。
只认一种的后果不是"少几个字段"，而是 `parse_post` 返回 `None` →
整页候选全废 → 报 `parse` 错误。上游 TS/Swift 也有这个兼容分支
（`mapTwitterUser` 的注释写着"兼容 legacy 与新版 core 结构"），照它做。

**H2. 视频/动图码率变体的键名是 `content_type`（snake_case），不是 `contentType`。**
实测：`"video_info": {"variants": [{"content_type": "video/mp4", ...}]}`（152 处）。
上游 TS/Swift 读的是 `contentType`——**照抄就会一个变体都读不到**，
于是视频选不出可下载 URL → `parse_media` 返回 `None` → `require_media` 把它筛掉 →
**整页变空**。两种拼写都要认。

**H3. 置顶推文是**单独一条指令** `TimelinePinEntry`，不在 `TimelineAddEntries` 里。**
实测 `user_tweets` 首页：3 条普通条目**都没有媒体**，唯一带媒体的那条是
`TimelinePinEntry` 里的置顶推文。只遍历 `TimelineAddEntries` 会把它整条丢掉。
（上游 TS 与 Swift 移植都没处理这条指令，所以这里与它们行为不同：我们多一条真实存在的推文。）

**H4. 被客户端筛空 ≠ 到底。**
`require_media=true` 时一页可能被筛成 0 条，但那是**我们筛的**，不是服务端没内容。
此时必须**继续返回游标**并且**不置 `end`**，否则爬虫会以为"没有更多了"提前停下。
判据只看服务端原始条数（§D5）。

**H5. 抓 `/search` 页面做 queryId 自愈时，**必须带凭据**。**
实测：不带 cookie 访问 `/search` 会被 307 到
`/i/jf/onboarding/web?...`，那个页面只有 17KB 且**没有任何 JS bundle 引用**，
正则必然失败；带 cookie 时返回 307KB 的真实 SPA 外壳，里面有 `main.<hash>.js`。
上游 Swift 注释说用 `cdnHeaders`（只带 UA）——**那条是错的**。

**H6. 当前 bundle 里的 `SearchTimeline` queryId 与代码里的默认值已经不同。**
实测（2026-10-01）bundle 里是 `uGB-gNd5HE4TkpO70OcFNw`，
而默认值 `Yw6L66Pw54NHKuq4Dp7b4Q` **仍然能用**（40 号那次采集就用的默认值）。
所以：默认值失效通常会表现为 404，由自愈兜住；**不要因为"和 bundle 不一样"就手工改常量**。

**H7. `following` 的默认页大小是 100，不是 20。** 上游就是这么写的，别"统一"成 20。

**H8. 两种主页模式是两个不同的 operation**（§B 侧记）：
`for_you` → `HomeTimeline`，`following` → `HomeLatestTimeline`。
它们共用同一份 `features`（与 `TweetDetail` 相同），但 queryId 与 operationName 都不同。

**E6. Aria2Next 对 404 会报"成功"。**
实测（Aria2Next 2.7.5）：`aria2.addUri` 一个不存在的 URL，`tellStatus` 返回
`status: "complete", errorCode: "0", completedLength: "0", totalLength: "0"`，
并在**磁盘上留下一个 0 字节文件**。
→ 这就是 §E2 那条"不能只看引擎的结论"的现场证据。
所以完成判据必须是**落盘字节数 + `expect_size` 校验**；
`crates/xspider-download/tests/aria2_e2e.rs` 有一条专门的测试钉住它。

**E7. Aria2Next 的 JSON-RPC 错误全是 `code: 1`。**
`Unknown option: x` / `GID deadbeef is not found` / `Unauthorized` —— 三个完全不同的原因，
返回的都是 `{"error":{"code":1,"message":"…"}}`。
→ **RPC 这一层没法按码分类**，也没法按文案分类（铁律）。
对策是把选项名在自己这边钉死（实测确认过的才用），并把 GID 记账在本地；
对 RPC 错误一律当"后端异常"上报，`message` 只作诊断。

**E8. Aria2Next 的 `--help=#all` 不完整。**
`--split`、`--max-connection-per-server` 在帮助里**看不到**，
但 `aria2.getGlobalOption` 读得到（值 6），也就是说它们是**可用**的。
→ 想确认某个选项存不存在，**问运行中的实例**（`getGlobalOption`），不要看帮助。
**E9. 媒体的字节数只有 CDN 知道；`码率 × 时长` 估出来的会差 5 倍。**
实测（2026-10-01，真实 CDN）：

| 事实 | 实测值 |
|---|---|
| GraphQL 的 `media` 对象里有大小吗 | **没有**。只有 `original_info`（宽高）与 `video_info`（时长/码率） |
| 图片：`HEAD` 的 `Content-Length` | ✅ 有（`https://pbs.twimg.com/media/….jpg` → 121661） |
| 视频：`HEAD` | ❌ 走不通（`SSL_ERROR_SYSCALL`）；**但 `Range: bytes=0-0` 稳定可用** |
| 视频：`Range: bytes=0-0` 的 `Content-Range` | ✅ `bytes 0-0/15187101` → 精确总长 |
| 图片尺寸参数的实际大小 | 无参数 = `?name=medium` = 121661；`?name=orig` = `?name=large` = 262165；`?name=small` = 49998 |
| **`码率 × 时长` 的估算误差** | 选中变体 10368000 bps × 61.494 s / 8 = **79696224 字节（76 MiB）**，而**真实是 15187101 字节（14.48 MiB）** → **差 5.25 倍** |

三条结论：
1. **要精确大小就去问 CDN**：先 `HEAD`，失败退回 1 字节的 `Range`（只下载 1 个字节）；
2. **不要拿码率估算当大小用**：码率是编码器上限，不是实际大小。拿它做"要不要外派给 aria2"
   的分界会一路判错（76 MiB 与 14.5 MiB 落在不同的分档里）；
3. **裸 URL 等价于 `?name=medium`（680px）**——这条与 §C6 对上了：
   我们返回的 `url` 不带 `?name=`，实际拿到的是 680px 图；要原图得自己加 `?name=orig`。

实现见 `crates/xspider-download` 的 `HttpDownloader::probe_size`：
**队列在下载前自动探测**（调用方没给 `expect_size` 时），于是媒体下载也能做完整性校验。
