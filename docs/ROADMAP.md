# 路线图与 M0 交付情况

> 里程碑定义与验收标准以 `docs/05-WORKFLOW.md` §2 为准。下列 M0–M4 表格保留 2026-10-01
> 的历史验收快照；本轮最新状态以文末 2026-10-05 台账为准。

---

## M0 地基 — 状态：**已完成**（2026-10-01）

验收标准（`docs/05` §2）与实际证据：

| # | 验收标准 | 状态 | 证据 |
|---|---|---|---|
| ① | `xspider_version` 在 cdylib 与 sidecar 两形态均可调用 | ✅ | `script/smoke.sh` 第 2/3 步；`script/cdylib_check.c` 用 `dlopen` + 三个符号实测；sidecar 的 ready 行 `"version":"1.0.0"` |
| ② | `fetch.get_user` 端到端（离线 fixture + live 各一次） | ✅ | 离线：`script/smoke.sh`（回放 fixture）；live：`script/smoke.sh` + `XSPIDER_LIVE=1`，真实返回 `id=13298072 screen_name=Tesla media_count=2041 register_time=2008-02-10T01:12:32Z` |
| ③ | `script/smoke.sh` 一条命令验证完 | ✅ | 单条命令跑完构建 → cdylib → sidecar 握手 → 调用 → 断言 → 关停 → 残留检查 |
| ④ | sidecar `--port 0` 打印 ready 行 | ✅ | ready 行在 stdout（并 flush），日志全部在 stderr；冒烟脚本断言 stdout 只有这一行 |

初始启动简报中的五项垂直切片验收物：

| # | 要求 | 状态 |
|---|---|---|
| 1 | 双形态可调 `xspider_version` | ✅ cdylib（dlopen）+ sidecar（ready 行 + `system.version`） |
| 2 | 一个真实 method（`fetch.get_user`）双形态都返回正确结果 | ✅ 19 个契约用例在两种形态下**逐字段一致**（`bins/xspiderd/tests/contract_dual.rs`） |
| 3 | 该 method 有一条离线 fixture 测试 | ✅ **3 条真实响应**（正常 / 不存在 / 未授权），经 `script/redact_fixtures.py` 脱敏入库 |
| 4 | sidecar 支持 `--port 0` 与 `--stdio` | ✅ 两者都实现；HTTP 为主 |
| 5 | `script/smoke.sh` | ✅ 见上 |

### M0 交付物清单

| 交付物 | 位置 |
|---|---|
| 契约（人读） | `docs/CONTRACT.md` |
| 契约（机器读） | `contract/xspider.schema.json` |
| 决策台账 | `docs/DECISIONS.md`（17 条 ADR） |
| 路线图 | 本文件 |
| 共享内核 | `crates/xspider-core`（HTTP / 凭据 / 限流熔断 / 签名 / 错误 / 时间） |
| 取数组件 | `crates/xspider-fetch`（`fetch.get_user`） |
| 下载组件 | `crates/xspider-download`（只有契约类型，引擎在 M2） |
| C ABI + 派发 | `crates/xspider-ffi`（`libxspider.*`） |
| sidecar | `bins/xspiderd` |
| 真实响应 fixture | `fixtures/`（含 `fixtures/README.md` 说明覆盖度与缺口） |
| 脚本 | `script/smoke.sh`、`script/cdylib_check.c`、`script/redact_fixtures.py` |

### M0 明确**没做**的事（有意为之，不是遗漏）

- **没有第二个端点**：初始 M0 简报要求垂直切片通过后再扩端点。
- **没有下载引擎**：M2 的活。`xspider-download` 现在只有契约形状的类型。
- **没有 `crawl.*` / `dl.*`**：已在 `CONTRACT.md` §3.3 登记形状，调用会得到 `invalid_request`。
- **没有 CI / 打包 / 签名**：M3。
- **没有 aria2 集成**：M2。
- **429 的真实 fixture 没录**：拿到真实 429 需要先把账号打进限流，**代价不对等**
  （拿使用者的账号配额换一条测试数据）。理由与替代方案见 `fixtures/README.md`。

---

## M1 取数 — 状态：**已完成**（2026-10-01）

验收标准（`docs/05` §2）逐条对照：

| # | 验收标准 | 状态 | 证据 |
|---|---|---|---|
| ① | 每端点 ≥5 类 fixture 样本全绿 | ⚠️ **部分** | 现有 **11 条真实 fixture**（正常/不存在/未授权/首页/翻页/两种主页模式/详情/搜索/关注），但不是每个端点都齐 5 类；缺口清单见下 |
| ② | 分页语义测试全绿（cursor 省略 / 推进 / 空页 / 游标未推进） | ✅ | `crates/xspider-fetch/tests/replay_offline.rs`；首页不带 cursor 由 `calls.rs` 测试钉住；"筛空不判到底"由 `a_page_emptied_by_filtering_still_carries_its_cursor` 钉住；"游标未推进"由 `xspider_core::paging::classify_cursor` 钉住 |
| ③ | 429 熔断与恢复有测试 | ✅ | `xspider-core/src/ratelimit.rs`：冷却生效、快速失败、退避放大、成功复位、上限封顶、**网络异常不留下粘性状态**（这是"代理恢复后还卡着"那条实测回归） |
| ④ | canary 能报出"哪个端点哪个字段" | ✅ | `crates/xspider-fetch/tests/canary_live.rs`：逐端点报告"阶段（请求/解析）+ 字段上下文 + 原始响应片段"，并在失败时自动存 `field_changed_<日期>.json`。**2026-10-01 实测 8 项全绿**（见下） |
| ⑤ | 内置 HTTP 下载后端 | ✅ | `crates/xspider-download` 的 `HttpDownloader` + **本地 HTTP fixture server 的 11 条 E2E**（正常 / Range 续传 / 断流重试 / 忽略 Range / 404 / 403 / 500 / 大小不符 / 撒谎的 Content-Length / 取消清理 / 未校验） |

### live canary 实测（2026-10-01，账号 tesla）

```
[ok] fetch.get_user: id=13298072 screen_name=Tesla media_count=Some(2041) register_time=Some("2008-02-10T01:12:32Z")
[ok] fetch.user_medias: 11 条，cursor=有，end=false
[ok] fetch.user_tweets: 12 条，end=false
[ok] fetch.tweet_detail: focal=2103296810735566850 回复 36 条（孤儿 0 条），cursor=有
[ok] fetch.search_timeline: 1 条（2026-09-24 ~ 2026-10-01），end=false
[ok] fetch.home_timeline(for_you): 28 条，end=false
[ok] fetch.home_timeline(following): 48 条，end=false
[ok] fetch.following: 52 人，end=false
canary 全绿：8 项
```

同时 `script/smoke.sh` 在 live 模式下也全绿（cdylib dlopen → sidecar 握手 → `fetch.get_user` → 关停 → 无残留）。

### 本轮交付物

| 交付物 | 位置 |
|---|---|
| 6 个新端点 | `crates/xspider-fetch`：`user_medias` / `user_tweets` / `tweet_detail` / `search_timeline` / `home_timeline` / `following` |
| 端点定义（queryId/features，逐字对齐上游） | `crates/xspider-fetch/src/endpoints.rs` |
| 请求构造的唯一实现点 | `crates/xspider-fetch/src/calls.rs` |
| 时间线遍历（广告过滤 / 去重 / 孤儿 / 置顶） | `crates/xspider-fetch/src/timeline.rs` |
| 推文与媒体 DTO | `crates/xspider-fetch/src/post.rs` |
| 推文详情（focal + 回复树） | `crates/xspider-fetch/src/tweet_detail.rs` |
| search queryId 自愈（锚定 operationName） | `crates/xspider-fetch/src/search_query_id.rs` |
| 分页原语（cursor 推进 / 到底 / 去重） | `crates/xspider-core/src/paging.rs` |
| 11 条真实 fixture + 脱敏脚本 | `fixtures/`、`script/redact_fixtures.py` |
| 契约与 schema | `docs/CONTRACT.md` §4.8–4.12、`contract/xspider.schema.json` |

### 本轮最重要的三个发现（都来自真实 fixture，不是推断）

1. **同一端点返回两种用户结构**（`legacy.*` 与 `core.*`/`avatar.*` 并存）——
   只认一种会让"半边端点"整页解析成空。见 `docs/02` §H1 / ADR-018。
2. **视频变体的键名是 `content_type`，上游代码写的是 `contentType`**——
   症状是"有视频却解析成空页"。见 `docs/02` §H2。
3. **置顶推文在单独的 `TimelinePinEntry` 指令里**，而它可能是整页唯一带媒体的那条。
   见 `docs/02` §H3 / ADR-021。

**这三条说明了一件事**：`docs/04` 那句"没有真实响应 fixture 就不许声称功能完成"
不是流程洁癖——本轮所有"看起来像端点逻辑坏了"的问题，根因全在响应形态上。

### M1 剩余工作

- [x] **内置 HTTP 下载后端**（`docs/01` §5.2）：流式落盘、Range 续传、断流重试、
      完整性校验、原子 rename、取消清理；用**本地 HTTP fixture server**（裸 TCP，可精确造断流）
      做了 11 条离线 E2E。**队列/状态机/事件/记录仍属 M2**——这里的公开面只有
      `HttpDownloader` + `DownloadRequest`，边界写在 crate 的模块文档里。
- [ ] fixture 覆盖度补齐到"每端点 5 类"：缺
      **空页（到底）**、**429**、**字段改名**三类。前两类可以安全地造
      （空页：翻到最后一页；429：不主动触发，等它自然出现或由本地 server 模拟）；
      第三类只能等 X 真的改版时由 canary 自动落盘。
- [x] `fetch.mutate`（点赞/转推/书签/关注，8 种 action）——**1.3.0 已实现**，
      成败看返回体（`errors[]`）而不只看状态码，见踩坑记录 38。
- [ ] 把 `x-rate-limit-*` 响应头接进限流状态（**主动**限流，而不只等 429）。
      fixture 里已留档（`normal.json` 的 headers）。
- [ ] 日期筛选的更多边界：跨月/跨年、单日区间已有测试；
      但"用户本地时区"这一层目前由外壳给定日历日期来保证（ADR 见 `CONTRACT.md` §4.11）。

**M1 已知风险**：

| 风险 | 影响 | 缓解 |
|---|---|---|
| 用户结构继续演进（第三种形态） | 又一个端点整页解析失败 | 解析用"候选路径"读字段；canary 会在改版的当天红 |
| queryId 轮换 | 搜索 404 | 默认值 + 自愈（带凭据抓页面）；自愈失败会报 `parse` 并带上下文 |
| 客户端筛选被误判成"到底" | 爬到一半就停 | ADR-020：`end` 只由服务端游标决定，已有测试 |
| fixture 覆盖度看起来"够了"但没测到过滤逻辑 | 广告/重复悄悄漏进结果 | `replay_offline.rs` 用**独立于实现**的遍历在 fixture 里找广告条目做交叉核对，并在"本次没测到"时明确打印 |

## M2 爬取与下载 — 状态：**已完成（2026-10-01 历史基线）**

> 本节表格记录 10 月 1 日时的覆盖现状。未完成任务状态持久化与恢复的缺口已纳入
> ADR-042 / 契约 1.5.2；当前实现与待补运行证据见文末 10 月 5 日台账。

**目标**：候选清单 + 策略参数 + `done_reason`、下载队列与状态机、`http` 内置后端、
aria2 外派后端、完整性校验。

验收标准：

| # | 标准 | 状态 | 证据 |
|---|---|---|---|
| ① | 本地 HTTP E2E（断点续传 / 断流重试 / 完整性失败 / 慢速 / 并发上限 / cancel 无残留） | ✅ | 队列并发上限与本地 HTTP 行为均有测试；本轮新增恢复路径验证仍在执行，见文末台账 |
| ② | `job_id` 幂等与重启恢复 | ✅（实现） | `already_known` 不发重复请求；version 1 记录持久化状态，Waiting/Active 恢复 Waiting，Paused 保持 Paused，Error（含取消）不自动重试。1.5.2 验收执行状态见文末台账 |
| ③ | 引擎选择按 `requirements` 生效 | ✅ | `segments > 1` → Aria2Next（内置后端做不到多连接）；小文件单分片 → 内置（省一个子进程）；无 Aria2Next 时**降级**而不是报错。策略留在组件内部（`docs/01` §5.2 纪律 1） |
| ④ | 下载记录由组件写、格式带版本字段 | ✅ | `RecordsFile{version:1}`；落盘用临时文件 + rename，**并发原子性由 1.5.1 修复**（同锁内快照 + 唯一临时名，ADR-041）；重启读回做对账（仅完成记录） |
| — | **`http` 内置后端** | ✅ | 11 条本地 HTTP E2E |
| — | **Aria2Next 外派后端** | ✅ | 8 条真二进制 E2E（含"404 被引擎报成成功"这条） |

### 本轮新增：Aria2Next 外派后端

`crates/xspider-download/src/aria2.rs`：子进程 + JSON-RPC（`addUri` / `tellStatus` /
`forceRemove` / `removeDownloadResult` / `shutdown`），启动时校验 `product=aria2-next`，
目录与文件名由调用方给定，完整性由我们自己校验，临时文件与控制文件由我们清理。

实测抓到的两个"引擎会骗人"的地方（都已写成测试）：

1. **404 会被报成 `status=complete` + `errorCode=0`，还留下 0 字节文件**——
   于是"完成"必须由落盘字节数判定；
2. **RPC 错误全是 `code: 1`**（Unknown option / GID not found / Unauthorized 一样）——
   所以 RPC 这一层不能按码分类，我们改为把选项钉死 + 本地记 GID，RPC 错误只作诊断。

### M2 剩余工作

- [x] **下载队列与状态机**：并发上限、`job_id` 幂等、暂停/恢复/取消、重启对账；
- [x] **事件上报**：`progress | completed | failed{reason} | skipped`
      （不单列 `integrity_failed`：它由 `completed.integrity` 与 `failed.reason` 表达）；
- [x] **下载记录**：由组件写、格式带版本字段（`docs/01` §5.1：两个写者必然出现静默不一致）；
- [x] **引擎选择策略**：按大小/可用性在 `http` 与 `aria2` 之间选（留在组件内部，不进契约）；
- [x] **爬取调度**：候选清单 + 策略参数 + `done_reason`（`docs/01` §4）;
- [ ] **`host` 逃生舱**：早期架构只登记了名称；method、字段和宿主执行模型尚未设计，不属于当前 26 个 method，也不是本轮承诺。

**M2 风险**：

| 风险 | 影响 | 缓解 |
|---|---|---|
| 用引擎输出的错误文案判断能否重试 | 引擎一换全错（真实反面教材） | 铁律禁止；分类只看结构化字段（ADR-016） |
| 引擎切换不丢弃断点 | 半成品拼接出**损坏文件**（`docs/02` §E3） | 引擎变了就丢弃断点，写成测试 |
| 非原子落盘 | 用户看到半个文件 | 临时文件 + rename；跨卷回退到复制。**并发原子性**（两任务同时完成时写整表快照）由 1.5.1 修复（ADR-041） |
| ~~未完成任务的记录不落盘~~ | ~~重启后无法恢复等待/暂停状态~~ | ADR-042 / 1.5.2 已实现 version 1 状态落盘与恢复；验证见本轮台账 |
| ~~环境目录未取得跨进程锁~~ | ~~共享目录的 sidecar 可能交错写入~~ | ADR-042 使用内核 `flock`，`--state-dir` 与 `XSPIDER_STATE_DIR` 都触发锁；保留锁文件 inode，不按路径存在判断活进程 |
| aria2 残留 `.aria2` 控制文件 | 换引擎就漏文件 | 清理由组件负责，不进"引擎内部细节" |
| 阻塞 IO 出现在 async 上下文 | sidecar 整体卡住 | `spawn_blocking`；clippy 盯不住，靠 code review |

---

## M3 分发 — 状态：**已完成（macOS 口径）**（2026-10-01，口径见 ADR-038）

**初始目标**：三平台 CI、打包脚本、签名、NOTICE/LICENSE、README；现行验收范围已按 ADR-038 / ADR-042 调整。

验收标准：

1. macOS CI 质量门通过；Windows/Linux 不在现行 CI 承诺中（ADR-038）；
2. macOS 上 cdylib 能被最小程序 `dlopen` 调用（**M0 已提前做掉**：`script/cdylib_check.c`）；
3. 无残留子进程的冒烟脚本（**M0 已做掉**）；
4. 产物目录结构见 `docs/03` §5。

**M3 实际范围与未覆盖项**：

- CI 使用 stable；ADR-011 因固定版本的 rustup 安装风险继续维持 stable；
- 产物名已经是 `libxspider.*`（ADR-012），打包脚本不必改名；
- macOS 产物必须 **ad-hoc 签名**（未签名会被内核 SIGKILL，表现为 137 静默死亡）；
- Aria2Next 若随包分发，必须附 LICENSE 与**源码获取方式**（GPL-2.0 合规），
  并在启动时校验 `--version`。仓库不 vendor 二进制；版本由外壳路径或显式打包输入提供。
- Android 构建、打包脚本与构建检查已纳入 ADR-042；它们不代替模拟器/设备运行、Aria2Next
  Android 二进制或 live 网络验收。运行证据由本轮跟踪台账补录。

---

## 参考实现审计（2026-10-01）— 状态：**已完成**

**目标**：本仓库是 `x-spider-mac` 的抽取，验收标准是"外壳只需略微改动就能接进来"。
所以按参考实现纪律逐项对照取数与下载两条路径；行为基准是接入前历史实现与备份分支。

产出（详见 `docs/DECISIONS.md` ADR-033/034/035 与踩坑记录 32–35）：

- **新增契约 method `net.probe_size`**（契约版本 1.0.0 → 1.1.0）：外壳可以在**决定下不下之前**
  按体积过滤，而不必自己再实现一套 CDN 探测；
- **修掉四个真实偏差**：下载请求缺 UA/Referer；外派后端从不传代理（需要代理的机器上全挂）；
  运行中换代理对下载无效；`requirements.segments` 此前是个空承诺（选项名在 Aria2Next 里已退役）。
  另有启动参数与默认阈值对齐、长推文正文截断、头像归一化三处。

**没做（有意）**：`size_hint` 仍然不进爬取候选（爬取阶段不探测，避免一页 20 个媒体白多 20 次请求）；
参考实现的 `_normal`/`_bigger` 之外的 UI 层处理（那是外壳的事）。

---

## M4 真实消费方 — 状态：**已完成**（2026-10-01）

**目标**：先用 CLI 当第一个真实调用方，再评估接入 `x-spider-mac`。

验收标准与证据：

| # | 验收标准 | 状态 | 证据 |
|---|---|---|---|
| ① | CLI 能完成"取一页 → 下 3 个媒体 → 报告结果" | ✅ | `bins/xspider-cli`（**不依赖任何 `xspider-*` crate**，只经契约的 JSON-RPC）。live 实测：`3/3 成功 · 0 失败 · 落盘 84.3 MiB · 用时 40.4s`，每个文件 `已校验`，字节数与 `net.probe_size` 逐字节相同；离线（fixture 回放）`--dry-run --json` 在 CI 里可跑 |
| ② | 契约在真实使用中未被推翻（若有变更记 ADR） | ✅ | 唯一新增的能力是 `net.probe_size`（ADR-033，写 CLI 之前就加了）。**消费者逼出来的修正**：`dl.events {since:0}` 被 `invalid_request` 挡回（契约说"从 0 开始"）、事件形状在 schema 里是空的、`expect_size: 0` 与 schema 的 `minimum: 0` 矛盾——见 ADR-036，前两条已修并有测试 |
| ③ | 记录"实际接入所需的改动清单" | ✅ | `docs/06-CONSUMER-INTEGRATION.md`：可删的（签名/自愈/闸门/解析/aria2 引擎/下载调度）、必须保留的（文件名模板、目录、UI、通知）、要新写的（sidecar 生命周期、凭据、代理热切换、transport 有界重试、事件轮询节流） |

**接入路径**（不变）：按 ADR-005，先走「外壳提供 aria2 路径」；
按 ADR-002，外壳切换形态只需换传输（HTTP ↔ cdylib），契约载荷不变。

**M4 留下的未决**：`system.version` 加 `transport` 字段 → **已做**（1.3.0）；
参考实现的"魔数/HTML 误页"完整性判定要不要搬进组件 → **已定：不搬**（`docs/06` §5 第 4 条）；
`net.set_limits` 的并发上限仍是启动时值 → **仍未做**。

**接入后审计发现的缺口**（2026-10-01，逐项对照 `x-spider-mac` 的 `pre-component-integration`；
完整对照表见 [`08-CAPABILITY-MAP.md`](08-CAPABILITY-MAP.md)）：

| 缺口 | 归属 | 影响 |
|---|---|---|
| ~~`post` 没有内嵌的 `quoted`~~ | **组件，已修（1.4.0）** | 当前契约与 DTO 有 `quoted`，见 `docs/08` §5.1 |
| ~~外壳的"继续"与"重试"重新入队~~ | **外壳，已修** | `already_known` 分支显式调用 `dl.resume`，见 `docs/08` §5.2 |
| ~~外壳启动时不与 `dl.list()` 对账~~ | **外壳，已修** | 启动无条件对账，并依产品策略暂停恢复的 waiting 任务，见 `docs/08` §5.3 |
| `crawl.run` 仅接入创建任务流程 | **外壳职责** | SyncStore 锚点日与主页视口填充仍由外壳实现；不属于组件缺口 |

---

## 总体风险台账

| 风险 | 等级 | 说明与缓解 |
|---|---|---|
| X 改版（queryId / features / 字段） | **高** | 这是常态而非意外。缓解：fixture 回放测试 + live canary + 错误带字段上下文 + `parse` 错误码专用于"该报警了" |
| 账号限流（429 风暴） | **高** | 单一闸门 + 按配额域分桶 + 429 不重试 + 冷却到期自然失效（没有任何"需要人工清除"的状态） |
| 契约漂移（文档与实现不一致） | 中 | 契约守卫测试：schema 的 method 集合 ⟷ 代码集合相等；错误字段与 DTO 字段都在测试里核对 |
| 凭据泄露 | 中 | 凭据只进不出：`Debug` 手工脱敏、访问器 `pub(crate)`、契约不回显、fixture 脱敏脚本带自检 |
| 代理/网络不稳定（实测已发生） | 中 | 代理可运行时切换（ADR-007）；进程内无粘性网络状态（ADR / 设计） |
| 本地环境工具链被打断安装 | 低 | ADR-011：不写死版本号 |

## 2026-10-05 安卓复用与审阅修复（已实施）

按 ADR-042 增量补齐 Android 专用 TLS、NDK 构建与 sidecar 打包脚本及 CI 构建检查，修复记录恢复与游标省略，并同步审阅指出的文档漂移。macOS 信任根与默认行为保持；Android 设备、Aria2Next 与 live 验证单独记录，不以构建通过代替。

本轮最终源码通过独立审阅；macOS 离线质量门、双形态/CLI smoke 与 release 打包签名检查全绿。Android arm64-v8a/x86_64 实际链接打包通过，arm64 API 36 普通应用 UID 下 HTTP/stdio/C ABI、32,791 字节本地下载及经 17890 的无凭据公开 TLS 探测通过。16 KB 仅 ELF/APK 对齐验证，实际设备为 4 KB 页；账号 GraphQL live、x86_64 设备、Aria2Next Android、后台生命周期和 Android CI 实跑仍未验收。命令与完整边界见 docs/10 §8；报告发现的归并处置见 docs/11。
