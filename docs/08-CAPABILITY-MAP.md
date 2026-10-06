# 08 · 能力地图：core 有什么、在哪实现、怎么测

> 本册描述 **core 自己**的能力：契约 1.5.2 的 26 个 method（外加 1 个 sidecar 传输层
> method `system.shutdown`）逐项给出「能力 → 契约 method → 实现位置 → 测试与 fixture 覆盖」。
> method 名称与数量以 [`CONTRACT.md`](CONTRACT.md) §3 与 [`09-METHOD-INDEX.md`](09-METHOD-INDEX.md)
> 为准，三者与 `contract/xspider.schema.json` 的 `method` 枚举的一致性由契约守卫测试
> （`crates/xspider-ffi/tests/contract_guard.rs`）钉住。
>
> 怎么调、入参出参、错误码见 [`07-API-REFERENCE.md`](07-API-REFERENCE.md)；
> 与参考实现 `x-spider-mac` 的逐项对照（迁移期审计）已归档到
> [`history/mac-integration-2026-10/CAPABILITY-PARITY.md`](history/mac-integration-2026-10/CAPABILITY-PARITY.md)。

---

## 1. 能力总表（26 个 method + 1 个传输层 method）

### 1.1 系统（`system.*`，2）

| 能力 | 契约 method | 实现位置（crate / 模块） | 测试与 fixture 覆盖 |
|---|---|---|---|
| 握手：契约版本 / 构建版本 / 当前形态（`sidecar` / `cdylib`） | `system.version` | 派发 `crates/xspider-ffi/src/engine.rs`；sidecar ready 行 `bins/xspiderd/src/server.rs` | `engine.rs::version_handshake_reports_both_versions`；双形态契约 E2E `bins/xspiderd/tests/contract_dual.rs`（sidecar HTTP 与 cdylib 走同一条派发路径） |
| 能力自检：列出本版本全部契约 method | `system.methods` | `crates/xspider-ffi/src/engine.rs`（`METHODS` 派发表） | `engine.rs::methods_list_is_not_empty_and_contains_the_ones_we_implement`；`contract_dual.rs::check_methods_are_reported_consistently`；守卫测试钉 method 枚举一致 |
| （传输层）sidecar 优雅退出：落盘状态、结束后端子进程 | `system.shutdown`（不属于 26 个契约 method，不进 `system.methods`） | `bins/xspiderd`（进程生命周期） | `contract_dual.rs`（退出后无子进程残留）；`script/smoke.sh` 离线冒烟（握手 / 调用 / 关停 / 无残留） |

### 1.2 凭据与网络（`auth.*` 2 + `net.*` 4）

| 能力 | 契约 method | 实现位置（crate / 模块） | 测试与 fixture 覆盖 |
|---|---|---|---|
| 注入凭据（只进不出，不落盘 / 不打日志 / 不回显） | `auth.set_cookie` | 派发 `engine.rs`；凭据持有与失效 `crates/xspider-core/src/creds.rs` | `engine.rs::set_cookie_never_echoes_the_credential`、`set_cookie_rejects_a_cookie_without_ct0`；`contract_dual.rs`（注入 / 缺 csrf 两分支） |
| 登录校验 / 当前账号（抓首页取 `screen_name`） | `auth.whoami` | `crates/xspider-fetch/src/social.rs` | `social.rs` 单元测试（从真实页面样本抽出三个字段）；**无 HTTP fixture / 回放 / canary 覆盖**（见 §3） |
| 限流参数：接口令牌桶 / CDN 并发 / 冷却秒数 | `net.set_limits` | `crates/xspider-core/src/ratelimit.rs`（闸门与熔断），`engine.rs` 接线 | `engine.rs::set_limits_requires_every_field`；`contract_dual.rs`（缺字段 / 正常各一）；令牌桶与 429 冷却行为见 `ratelimit.rs` 单元测试 |
| 运行中换 / 关代理（取数与下载一起换，派发那一刻生效） | `net.set_proxy` | 入参解析 `crates/xspider-core/src/http.rs`；客户端热替换 `crates/xspider-core/src/stack.rs`；下载侧 `crates/xspider-download/src/queue.rs`、`http_backend.rs` | `engine.rs::set_proxy_accepts_url_and_null`；`contract_dual.rs`（null / 漏字段）；`queue.rs::proxy_can_be_swapped_at_runtime` |
| 限流状态查询（是否处于冷却） | `net.status` | `crates/xspider-core/src/ratelimit.rs`，`engine.rs` 派发 | `engine.rs::status_starts_ok`；`contract_dual.rs`（初始 + 跑过之后各一次） |
| 问 CDN 媒体大小（HEAD，失败退 1 字节 Range；一次至多 1 字节流量） | `net.probe_size` | `crates/xspider-core/src/stack.rs`（probe）；下载队列同语义的下载前探测（`probe_size_when_unknown`）`crates/xspider-download/src/http_backend.rs`、`queue.rs` | `http_e2e.rs`（HEAD 给 Content-Length / Range 兜底 / 服务端不给 → `null` / 探测值回填 `expect_size`）；`engine.rs::probe_size_offline_says_unknown_without_touching_the_network`（回放模式不联网） |

### 1.3 取数（`fetch.*`，9）

| 能力 | 契约 method | 实现位置（crate / 模块） | 测试与 fixture 覆盖 |
|---|---|---|---|
| 用户资料（`legacy` / `core` 两种响应形态都认） | `fetch.get_user` | `crates/xspider-fetch/src/user.rs` | fixture `user_by_screen_name/{normal,not_found,unauthorized}`（正常 / 200 空 data / 匿名 403 空体）；`replay_offline.rs` + `engine.rs`（`@` 前缀归一化、not_found、未注入凭据 → `unauthorized`）+ live canary |
| 媒体时间线（单页 + 游标） | `fetch.user_medias` | `crates/xspider-fetch/src/timeline.rs` | fixture `user_medias/{page1,page2}`；回放：游标推进、全页 id 唯一、任何时间线 fixture 无广告条目；canary |
| 推文时间线（`require_media` / `include_retweets` 两开关） | `fetch.user_tweets` | `crates/xspider-fetch/src/timeline.rs` | fixture `user_tweets/page1`；回放：严格模式只留带媒体推文；canary |
| 推文详情 + **扁平**回复列表（`parent_id` / `is_partial_parent`） | `fetch.tweet_detail` | `crates/xspider-fetch/src/tweet_detail.rs` | fixture `tweet_detail/with_replies`；回放：focal 与孤儿回复标记；canary |
| 按日期搜某人推文（`until` 含当天，内部 +1 天） | `fetch.search_timeline` | `crates/xspider-fetch/src/search.rs`；queryId 自愈 `search_query_id.rs`（锚定 `operationName`，404 才自愈） | fixture `search_timeline/media_only` + 真实 bundle 原料 `query_id_source.json`；`search_query_id.rs` 单元测试；canary |
| 主页时间线（`for_you` / `following` 两模式） | `fetch.home_timeline` | `crates/xspider-fetch/src/timeline.rs` | fixture `home_timeline/{for_you,following}`（两种 operation 各一页）；回放：两种模式；canary（两模式各一次） |
| 关注列表 | `fetch.following` | `crates/xspider-fetch/src/timeline.rs` | fixture `following/page1`；回放：用户解析；canary |
| 关注态（v1.1 `friendships/show`；缓存"我是谁"，换 cookie 自动失效） | `fetch.is_following` | `crates/xspider-fetch/src/social.rs` | **本仓库未发现专属离线用例**（无 fixture / 回放 / canary，2026-10-06 核对）；结构对不上时报 `parse`，不默默返回 `false` |
| **写操作**：点赞 / 转推 / 书签 / 关注（8 种 `action`） | `fetch.mutate` | `crates/xspider-fetch/src/social.rs`（`ensure_mutation_succeeded` 按**响应体** `errors[].code` 判定，不看 HTTP 状态码） | `social.rs` 单元测试：8 种 action 解析、variables 形状对齐参考实现、成功与否按 body 判定、从真实页面样本抽字段；**无 HTTP fixture / canary**（写操作会对真实账号生效） |

### 1.4 下载（`dl.*`，8）

| 能力 | 契约 method | 实现位置（crate / 模块） | 测试与 fixture 覆盖 |
|---|---|---|---|
| 入队下载（`job_id` 幂等；记录落盘后才派发） | `dl.enqueue` | `crates/xspider-download/src/queue.rs` | `queue_e2e.rs::queue_downloads_and_writes_a_record`、`enqueue_does_not_accept_or_dispatch_when_record_write_fails`；`queue.rs` 单元测试（同 `job_id` 只收一次、空 URL 拒收、记录写失败不入队）；`contract_dual.rs`（缺字段 / `file_name` 带路径） |
| 暂停（**保留**断点） | `dl.pause` | `crates/xspider-download/src/queue.rs` | `queue_e2e.rs::pause_keeps_the_partial_and_resume_finishes_the_download`；`rapid_pause_then_resume_serializes_the_old_backend_run`（旧轮不能写新轮的 `.part`）；`cancel_after_pause_waits_for_cleanup_before_explicit_resume` |
| 恢复（对 `paused` / `error` 显式恢复；`waiting`/`active` 是成功但无操作） | `dl.resume` | `crates/xspider-download/src/queue.rs` | `queue_e2e.rs::resume_on_an_active_job_is_a_successful_noop`、`paused_job_survives_restart_and_resumes_with_saved_parameters`；`queue.rs::stale_dispatch_cannot_invalidate_a_newer_resume`（过期派发作废不了新恢复） |
| 取消（**丢弃**断点；在飞任务真正停下才落终态） | `dl.cancel` | `crates/xspider-download/src/queue.rs` | `queue_e2e.rs::cancel_discards_the_partial_and_marks_the_job_failed`、`cancel_cleanup_failure_keeps_intent_pending_until_a_retry_succeeds`、`pending_cancel_recovery_cleans_part_but_preserves_ambiguous_final_file`（不能证明归属的完整文件不误删） |
| 单任务快照 | `dl.status` | `crates/xspider-download/src/queue.rs` | `queue.rs::unknown_job_id_is_a_structured_not_found`；`contract_dual.rs`（未知任务 → `not_found`） |
| 全部任务快照（**重启对账的权威来源**，按 `job_id` 排序） | `dl.list` | `crates/xspider-download/src/queue.rs` | `queue_e2e.rs::restart_is_reconciled_through_the_records_file`、`interrupted_active_record_recovers_from_the_disk_part`、`legacy_version_one_record_recovers_with_default_requirements`；`queue.rs`（记录跨重启存活并报 version、损坏记录不弄垮队列、未来 version 忽略不猜） |
| 增量事件（带游标轮询，`since` 首次传 0；不是推送流） | `dl.events` | `crates/xspider-download/src/queue.rs` | `queue.rs::events_are_retrievable_by_cursor`；`queue_e2e.rs::integrity_failure_surfaces_in_state_events_and_cleanup`；事件只属于本次调用的边界由 `engine.rs::crawl_run_events_cover_only_this_call_and_seq_never_goes_backwards` 钉住 |
| 清掉已结束任务的内存快照 | `dl.prune` | `crates/xspider-download/src/queue.rs` | `queue_e2e.rs::finished_jobs_can_be_pruned`（语义注意见 §3） |

### 1.5 爬取（`crawl.run`，1）

| 能力 | 契约 method | 实现位置（crate / 模块） | 测试与 fixture 覆盖 |
|---|---|---|---|
| 跑一轮爬取：候选清单（有损）+ 同批完整推文（`posts[]`）+ `done_reason` 终止判据族（`exhausted` / `time_progressed` / `empty_pages` / `cursor_stuck` / `wanted_collected` / `page_limit_reached` / `cancelled` / `error`） | `crawl.run` | `crates/xspider-download/src/crawl.rs` | `crawl.rs` 单元测试：四条终止判据、陈旧账号报 `time_progressed` 而非空页、`wanted` 收齐即停、去重先于筛、同一媒体在两条推文里是一个候选、无 `created_at` 保留、`until` 含整天、媒体类型筛、逐页游标、取消、按页节流、`max_pages` 上限、页失败保留原错误码、取页错误码穿透；`engine.rs`（candidates 旁给 `posts[]`、事件只覆盖本次调用且 `seq` 不回退、耗尽时省略 `next_cursor`）；`contract_dual.rs`（`source` 不合法） |

---

## 2. 共享内核与下载引擎（随上面两组 method 生效，不单独成 method）

| 能力 | 实现位置（crate / 模块） | 测试与 fixture 覆盖 |
|---|---|---|
| 限流闸门与 429 熔断（接口 / CDN 两套配额分别治理） | `crates/xspider-core/src/ratelimit.rs` | `ratelimit.rs` 单元测试（令牌桶、突发、冷却、`Retry-After` 解析） |
| 请求签名（`x-client-transaction-id`）与失效重取 | `crates/xspider-core/src/xclid.rs` | `xclid.rs` 单元测试 + 真实页面原料 `fixtures/xclid/page_artifacts.json`；回放模式跳过签名（真实签名的正确性交给 live recorder / canary） |
| 传输层：有界重试、错误分类（状态码定大类 + 结构化字段定细分）、两种时间格式 | `crates/xspider-core/src/transport.rs`、`error.rs`、`xdate.rs` | 各自单元测试（时间格式用已知纪元秒断言） |
| fixture 回放（离线默认，不碰网络） | `crates/xspider-core/src/fixture.rs` | `replay_offline.rs`（11 条用例，含"入库 fixture 全部脱敏""分页 fixture 声明游标期望"两条资产自检）；`contract_dual.rs::check_fixtures_are_actually_exercised` |
| 内置 HTTP 下载后端（流式落盘、Range 续传、断流重试、原子 rename、取消清理、UA/Referer/Accept-Encoding: identity） | `crates/xspider-download/src/http_backend.rs` | `http_e2e.rs` 15 条：字节精确、大小不符即完整性失败且无残留、断流从已写字节续传、服务端无视 Range 则重启、永久截断失败并清理、404/403 不重试、取消无残留、无 `expect_size` 明示 Unverified、既有断点续接、说谎的 Content-Length 判失败 |
| Aria2Next 外派后端（JSON-RPC 记账、逐任务显式传代理与 UA/Referer、控制文件断点） | `crates/xspider-download/src/aria2.rs` | `aria2_e2e.rs` 8 条：下载并校验完整性、**404 不得报成功**（引擎说成功不算）、大小不符清理、取消无残留、不混用别引擎断点、拒绝上游 aria2、关停无子进程残留、drop 即杀子进程 |
| 引擎选择（按 `requirements.segments` + 探测到的真实大小）与下载前探测 | `crates/xspider-download/src/queue.rs`、`http_backend.rs` | `queue_e2e.rs::multi_segment_requirement_degrades_gracefully_without_aria2`（无 aria2 时优雅降级）、`unknown_size_is_probed_so_integrity_is_still_verified` |
| 并发上限（队列配置）与页间节流 | `crates/xspider-download/src/queue.rs`、`crawl.rs` | `queue_e2e.rs::concurrency_is_capped_by_the_configured_limit`（按"正在写字节"计）、`crawl.rs::page_throttle_is_respected` |
| sidecar HTTP / stdio 两种传输；state-dir 单实例锁（Unix 内核 `flock`，锁文件保留 inode） | `bins/xspiderd/src/server.rs`（HTTP + ready 行）、`rpc.rs`（请求解析 / stdio JSON Lines / token）、`lock.rs` | `rpc.rs` 单元测试（最小请求解析、畸形 body 结构化报错不 panic、缺 method、token 头或 body 均可）；`lock.rs` 单元测试（在持者互斥、陈旧 pid 不挡启动、释放保留 inode）+ `bins/xspiderd/tests/state_dir_lock.rs`（env state-dir 独占、进程死亡后释放）；`contract_dual.rs` 钉 sidecar HTTP 全流程；stdio 形态的设备实测记录见 [`10-ANDROID-INTEGRATION.md`](10-ANDROID-INTEGRATION.md) §8 |
| 三层契约一致性守卫（METHOD 派发表 = schema 枚举 = CONTRACT 文档 = `docs/09`） | `crates/xspider-ffi/tests/contract_guard.rs` | `schema_method_enum_equals_implemented_methods`、`schema_details_cover_every_method`、`contract_markdown_documents_every_method`、`error_codes_in_schema_match_the_code_enum`、`every_error_field_is_declared_in_the_schema` |
| 第一个真实消费方（只经契约，不链接任何本仓库 crate） | `bins/xspider-cli` | `only_the_contract.rs`（依赖守卫 + 主传输形态演练）；`dry_run_offline.rs`（离线规划 / JSON 报告 / 用法错误 / sidecar 缺失是 transport 失败而非静默成功 / CLI 调的 method 都被组件声明）；`script/smoke.sh` 离线全链路 |

---

## 3. 当前缺口

**组件侧尚未实现 / 未设计**（调用会得到 `invalid_request` 或无此入口）：

| 项 | 状态 |
|---|---|
| `dl.plan` / `dl.report` | 早期设计只留下 `host` 名称；method、字段形状及宿主执行模型均未设计，不在当前 26 个 method 中（`CONTRACT.md` §3.3 只登记预留名称） |
| `net.set_limits.cdn_concurrency` 运行中不可变 | 信号量不能缩容，只在队列创建时生效；"运行中调并发"需要另外的接口（这是外壳侧仍待决定的产品行为，见 [`06`](06-CONSUMER-INTEGRATION.md) §5 第 5 条） |
| 推送式事件流 | 不做（三种形态统一用游标轮询，ADR-029） |

**测试覆盖缺口**（2026-10-06 核对）：

- `auth.whoami` / `fetch.is_following` / `fetch.mutate` 没有专属的 HTTP fixture、回放用例或
  live canary（canary 只覆盖 7 个取数端点）。三者的解析与判定逻辑有单元测试钉住
  （`social.rs`），但**它们对真实 X 响应的端到端行为没有被离线资产自动化**。

**容易想反的语义**：

- `dl.prune` **只清内存快照，不清下载记录**：配了 `--state-dir` 时记录仍在，清完之后
  同一个 `job_id` 再入队**仍然**是 `already_known`（`enqueue` 在内存里查不到之后还会去查记录）。
  要重下必须换新的 `job_id`。
- 完整性只管"**字节对不对**"；"这个文件真的是图片 / mp4 吗"（魔数、HTML 误页）
  有意留在外壳——分工与理由见 [`06`](06-CONSUMER-INTEGRATION.md) §5 第 4 条。

**运行时未验收边界**：Android 设备 / 16 KB 页 / Aria2Next Android / Doze 等仍开放的验收边界
以 [`10-ANDROID-INTEGRATION.md`](10-ANDROID-INTEGRATION.md) §8 的记录为准，不以编译或模拟器通过替代。

**已闭合的历史缺口**（暂停/恢复接线、重启对账、`quoted` 内嵌、`crawl.run` 接入外壳等
当时外壳侧的处置）见 [`history/mac-integration-2026-10/`](history/mac-integration-2026-10/MIGRATION.md)。

---

## 4. 有意不进组件的事（分工，不是缺口）

| 事项 | 理由 |
|---|---|
| 文件名模板、目录选择 | 产品语义，`file_name` / `dest_dir` 由外壳算好传入（CLI 在 `bins/xspider-cli/src/plan.rs` 有可抄的默认模板示范） |
| UI 取图（头像 / 缩略图） | 图片展示不属于契约范围；走外壳自己的 HTTP 栈更简单 |
| 登录 UI 与 cookie 获取 | 登录是外壳的事，cookie 由外壳经 `auth.set_cookie` 注入（只进不出） |
| "健康探针"类的 CDN 探测 | 产品行为；组件侧对应的权威信息是 `net.status` 与错误的 `code` |
| 魔数 / HTML 误页判定 | 见 §3"容易想反的语义" |
| 视口填充式滚动、通知、进度节流 | 渲染与体验问题，事件轮询 + `dl.list` 已给外壳足够信息 |
