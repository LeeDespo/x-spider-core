# CHANGELOG

本文件记录**对外可见**的变化：契约、行为、产物。内部重构不写在这里。

格式参照 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)；
版本遵循[语义化版本](https://semver.org/lang/zh-CN/)。
契约版本的兼容规则见 [`docs/CONTRACT.md`](docs/CONTRACT.md) §6：
**method 与字段只增不改不删**。

## [未发布]

### 文档
- **`docs/07-API-REFERENCE.md` 重写为完整接口参考**：26 个契约 method 逐个给出
  入参表（类型/必填/默认）、出参、示例与易错点；新增"数据形状"一节
  （`post` / `media` / `page` / `reply` / `jobSnapshot` / `downloadEvent` / `candidate` 的逐字段说明）、
  错误与重试纪律、四份可直接照抄的调用时序。
- **新增 [`docs/08-CAPABILITY-MAP.md`](docs/08-CAPABILITY-MAP.md)**：组件与 `x-spider-mac`
  （tag `pre-component-integration`）的**逐项能力对照**，以及仍未覆盖 / 未接线的部分。
- **新增 [`docs/09-METHOD-INDEX.md`](docs/09-METHOD-INDEX.md)**：26 个 method 的入口索引，
  详情链接直达 `docs/07` 的对应小节。
- **新增 [`docs/10-ANDROID-INTEGRATION.md`](docs/10-ANDROID-INTEGRATION.md)**：Android 构建、
  sidecar 部署与外壳职责；明确构建通过不等于模拟器、设备或 live 验收。

### 契约（1.5.2 PATCH，ADR-042）
- `crawl.run` 没有下一页时省略 `next_cursor`，与可选字段约定一致。
- schema 补齐已有 `crawl.run` 计数字段与事件形状，并将 `auth_required` 纳入 reason 说明；这些是对既有输出的准确描述，没有增加 method、错误码或输出字段。
- 下载状态写入 version 1 记录并在重启时恢复；Waiting/Active 恢复为 Waiting，Paused 保持暂停，
  Error（包括取消）保持错误且不自动重试。`dl.prune` 仍只清内存快照，不清持久记录。
- Android 使用 webpki 公共根证书，macOS 原生信任根保持；新增 Android NDK 构建、打包和 CI 检查。
  当前没有据此宣称 Android 真机、Aria2Next Android 资产或 live 网络验收通过。
- 保留 26 个 method 与 3 个 C ABI 导出，不新增契约字段或方法。

### 新增
- **`xspider-cli`**（`bins/xspider-cli`）：M4 的"第一个真实消费方"。
  它**不依赖任何 `xspider-*` crate**（守卫测试强制读它自己的 `Cargo.toml`），
  只经契约的本地 JSON-RPC 使用组件，因此它是"用别的语言写的外壳"的替身——
  契约缺什么、哪里别扭，它第一个炸。跑通的就是 M4 验收那条链路：
  取一页 → 按 `net.probe_size` 看体积 → 下 N 个媒体 → 报告结果。
  支持 `--dry-run`（离线 fixture 可跑，CI 用）、`--json`（机器可读报告）、
  `--list-methods`（外壳的启动自检）；退出码 0 成功 / 1 有媒体没下成 / 2 用法错 / 3 契约失败。

### 契约
- **契约版本 → 1.5.1**（PATCH，**不增删 method / 字段 / 错误码**）：四个修复。
  ① `crawl.run` 的取页失败**原样透出**原始结构化错误（`code` 与
  `retry_after_s` / `status` / `context` / `endpoint` / `detail`），只在 `message` 里附页号，
  不再一律吞成 `internal`；② `crawl.run` 的 `seq` / `events` 收敛为**本次调用**的事件
  （`seq` 仍进程内单调、不回退）；③ sidecar 的 `--state-dir` 与 `XSPIDER_STATE_DIR`
  都决定下载记录路径（**flag 优先**）；④ 下载记录落盘的并发原子性修复。
  ①②③ 是把实现修回契约早已承诺的行为；④ 是纯内部修复，对外不可见。
  （`posts[]` 的加法是 1.5.0，见下条。本仓库惯例：契约版本变更统一列在 [未发布]，
  直到切出下一个构建版本，故 1.5.1 与 1.5.0 同处本节、各自成条、互不覆盖。）
- **契约版本 → 1.5.0**：`crawl.run` 的结果新增 **`posts[]`**——这一轮**保留的完整推文**
  （与 `candidates` 是同一批数据的两个视角）。起因是接真实外壳时暴露的一个设计缺口：
  候选是**有损**的（只有 url 与几个标量），而外壳要按用户的文件名模板命名、把任务写进
  自己的历史记录，需要正文、作者昵称/id、标签、媒体宽高与页内序号。少了它，`crawl.run`
  对任何"要命名/要记账"的外壳都不可用——第一个消费方（CLI）不要名字，所以之前没暴露。
  **纯加法，不改不删。**
- **契约版本 → 1.4.0**：`post` / `reply` 新增 **`quoted`**（内嵌的被引用推文，**只嵌一层**）。
  此前只有 `quoted_id`，接入后引用推文在界面上只剩一个空壳——参考实现有 `mapQuotedPost`，
  抽取时把这一层漏了（现归档于 `docs/history/mac-integration-2026-10/CAPABILITY-PARITY.md` §5.1）。
  取不到（被删/不可见）时该键不出现，`quoted_id` 仍在。**纯加法，不改不删。**
- **写操作的 141 归到 `unauthorized`**：X 用 141 表达"这个账号被限制写操作"
  （`User is suspended, deactivated or offboarded`）。实测：一个 0 推文、0 关注的账号
  读全部正常、写全部 141。归到 `unauthorized` 是因为**外壳该做的事与登录失效一样**。
- **契约版本 1.0.0 → 1.1.0 → 1.2.0 → 1.3.0**：只增 method/字段，不改不删（`docs/CONTRACT.md` §6）。
- **新增 3 个 method**（1.3.0）：`auth.whoami`（登录校验，替代"外壳自己抓首页正则"）、
  `fetch.is_following`（关注态；组件内部缓存"我是谁"，换 cookie 自动失效）、
  `fetch.mutate`（**写操作**：点赞/转推/书签/关注，8 种 action）。
  搬进来之后，外壳**不再需要自己的 HTTP 层**（签名、限流、重试、错误分类全在组件）。
- **`media` 新增 `poster_url`**（1.2.0）：封面图（X 的 `media_url_https`）。
  视频/动图以前只给"可下载的 mp4"，而界面需要的是封面 —— 两者混用的后果是
  **视频格子整片空白**（用户实测报回来的问题，见 ADR-039）。`url` 仍是可下载地址。
- **新增 method `net.probe_size`**：`{url} → {size}`。问 CDN"这个文件多大"，
  一次调用最多产生一个字节的流量；`size: null` 表示服务端没说（回放模式下恒为 `null`）。
  用途是让外壳在**决定下不下之前**按体积过滤（`docs/DECISIONS.md` ADR-033）。
  这与 `dl.enqueue` 内部的自动探测（ADR-032）不冲突，后者仍是缺省行为。
- **`dl.events` 的事件形状进了 schema**：新增 `$defs/downloadEvent`（`oneOf` 四种，
  各自 `additionalProperties: false`）与 `$defs/integrity`，`items` 指向它，
  并有契约守卫测试盯字段集合。此前机器可读契约在这里写的是
  `{"items": {"type": "object"}}`——等于把"猜"留给每个消费方。

### 修复（契约 1.5.1）
- **`crawl.run` 的取页失败被吞成 `internal`**：翻页中某一页取数失败时，
  `unauthorized` / `rate_limited` / `not_found` / `parse` / `invalid_request` /
  `upstream` / `transport` 全被替换成 `internal`，外壳分不清"重新登录 / 退避 /
  参数写错 / 组件 bug"。现在保留原始 `code` 与结构化字段，页号只写进 `message`。
  **不改 error 字段集合**，schema 与契约守卫不动。
- **`crawl.run` 返回进程级累积事件**：`events` 此前取的是进程启动以来**所有**轮次的事件，
  而文档写的是"本轮"；外壳按推荐方式"小页数反复调用"会重复收到历史事件。
  现在 `seq` / `events` 只覆盖**本次调用**，`seq` 仍进程内单调、不回退。
- **`--state-dir` 不决定下载记录路径**：此前它只用于实例锁，记录路径只认
  `XSPIDER_STATE_DIR`，而文档写"下载记录与重启对账都在 `--state-dir` 里"。
  现在两者都决定记录路径，**`--state-dir` 优先**（并仍启用单实例锁）。
- **下载记录落盘不是原子的**（对外不可见）：先 insert 解锁、再重新 clone 整个记录表写盘，
  两次加锁之间别的任务 insert 会写出缺一条的旧快照；并发写者还共用同一个 `.tmp`。
  现在"快照 + 写盘"在同一把写锁内完成，并用唯一临时文件名再 rename。
  对外可观测行为不变（`downloads.json` 格式 / `RECORDS_VERSION` 不变）。

### 修复
- **`xspiderd` 现在真的认 `XSPIDER_COOKIE`**：此前文档（与 `AGENTS.md` 的常用命令）都写了
  "启动时注入凭据"，而实现里只有测试读这个变量，sidecar 根本没实现——文档在教一件不存在的事。
  现在启动时注入，等价于先调一次 `auth.set_cookie`；**只进不出**，不打日志、不回显。

### 修复（M4 的消费者逼出来的）
- **`dl.events {since: 0}` 被拒**：契约写的是"从 0 开始"，而实现把 0 当非法值挡回。
  症状最坏——前 3 个任务已入队，才在轮询第一步炸。`optional_u64` 现在允许 0，
  另设 `optional_positive_u64` 给 `count` 这类"0 说不通"的字段；
  `expect_size: 0` 也与 schema 的 `minimum: 0` 对齐（见 ADR-036）。

### 修复（对照 `x-spider-mac` 的审计结果）
- **下载请求缺 `User-Agent` 与 `Referer`**：两个后端现在都带与参考实现相同的
  UA（Chrome 142 macOS）与 `Referer: https://x.com/`，并显式 `Accept-Encoding: identity`
  （避免压缩导致"声明大小 ≠ 落盘字节数"的完整性**假红**）。
- **外派后端从不传代理**：Aria2Next 是子进程，不认识 `HTTPS_PROXY`。
  现在逐任务带 `all-proxy`（由 `ProxyConfig::resolve_url()` 解析成具体 URL）。
  此前在"需要代理才能出网"的机器上，内置后端正常、外派后端全挂。
- **运行中换代理对下载无效**：下载器不再在构造时快照代理；
  `net.set_proxy` 会同时切换两个后端，且代理在**派发那一刻**读取（ADR-035）。
- **`requirements.segments > 1` 此前是个空承诺**：现在映射到 Aria2Next 的
  `stream-max-connections`（上游 aria2 的 `--split` / `--max-connection-per-server`
  在这个 fork 里已退役，照抄旧名字等于没设）。
- **引擎启动参数**补上 `--conf-path=/dev/null` 与 DHT/BT 相关全关（不开 UDP 端口，
  避免防火墙弹窗）；`min_size_for_aria2` 默认 5 MiB，与参考实现的默认阈值一致。
- **长推文的正文会被截断**：正文优先取 `note_tweet.note_text`，并按参考实现清洗
  （去掉媒体占位链接、短链换 `expanded_url`）。
- **头像归一化**：`//` → `https://`、`_normal` → `_bigger`，与参考实现给外壳的值一致。
- **`media_count` 在新结构里读 `core.tweet_counts.media_tweets`**（此前只读 `legacy`）。

### 变更
- **CI 收敛为 macOS 单平台**：`fmt + clippy -D warnings + test`、cdylib 的 `dlopen` 验证与
  冒烟脚本现在都只跑 **macOS** 一格；Linux / Windows 的 job 已删除——它们没有真实消费方，
  跑出来的绿是**假的覆盖率**，比没有更坏（ADR-038）。跨端由"不引入平台专有依赖"的
  代码级纪律（不用 native-tls、不用 Apple 专有框架、路径不假设 POSIX）保证，仍由 clippy 与测试守着。

### 计划中
- `dl.plan` / `dl.report`：`host` 逃生舱（iOS 后台 `URLSession`、App Store 沙箱外壳）。
- 下载队列的推送式事件流（当前是"带游标的增量轮询"，见 `docs/DECISIONS.md` ADR-029）。
- `net.set_limits` 的 `cdn_concurrency` 目前只在队列创建时生效（信号量不能缩容）。

## [0.1.0] — 2026-10-01

首个版本：M0（地基）、M1（取数）、M2 的取数+下载引擎、M3 的分发产物。

### 契约（`contract/xspider.schema.json`，契约版本 `1.0.0`）
- 入口固定为三个 C ABI 函数：`xspider_version` / `xspider_call` / `xspider_free`。
- 22 个 method：`system.*`、`auth.set_cookie`、`net.*`、`fetch.*`（7 个端点）、
  `dl.*`（8 个）、`crawl.run`。
- 结构化错误对象（9 个 code）+ `additionalProperties: false` 的 schema；
  契约守卫测试保证文档 / schema / 代码三者一致。

### 新增
- **`xspider-core`**：HTTP 栈、凭据只进不出、限流闸门与 429 熔断（按配额域分桶）、
  `x-client-transaction-id` 签名（含 queryId 自愈里的锚定纪律）、错误分类、分页原语、时间归一化。
- **`xspider-fetch`**：`get_user` / `user_medias` / `user_tweets` / `tweet_detail` /
  `search_timeline` / `home_timeline` / `following`。
  广告过滤（三个入口）、跨页去重、转推展开、孤儿回复标记、置顶推文（`TimelinePinEntry`）、
  搜索 queryId 自愈（**锚定 `operationName`**）、兼容两种用户结构（`legacy` / `core`）。
- **`xspider-download`**：
  - `http` 内置后端：流式落盘、Range 续传、断流重试、完整性校验、原子 rename、取消清理；
  - **Aria2Next** 外派后端：子进程 + JSON-RPC，启动时校验 `product=aria2-next`；
  - 下载队列：并发上限、`job_id` 幂等、暂停/恢复/取消、事件、**带版本字段的下载记录**、
    引擎选择（留在组件内部）；version 1 的未完成状态重启恢复由 1.5.2 增补；
  - **大小探测**：`expect_size` 缺省时自动问 CDN（`HEAD`，失败退回 1 字节 `Range`），
    于是媒体下载也做得到完整性校验。实测：码率估算会差 5 倍，问才是对的；
  - 爬取调度：候选清单 + 策略参数 + `done_reason`（时间轴推进为主判据）。
- **`xspiderd`**（sidecar）：`--port 0` 的 ready 行握手、随机 token、`--stdio` 备选、
  单实例锁、优雅退出；**stdout 只用于握手，日志全走 stderr**。
- **`libxspider.dylib` / `.so` / `.dll`**（cdylib）：三个 C ABI 函数，产物名与文档一致。
- 测试资产：13 条**真实抓取并脱敏**的 fixture、本地 HTTP fixture server（能造断流）、
  Aria2Next 的真二进制 E2E、live canary（逐端点报告"阶段 + 字段上下文"）。
- 脚本：`script/smoke.sh`（一条命令验证双形态 + 端到端 + 无残留进程）、
  `script/redact_fixtures.py`（**自带四条事后断言**）、`script/package.sh`、
  `script/cdylib_check.c`。
- CI：`fmt + clippy -D warnings + test` + cdylib 的 dlopen 验证。初版曾规划三平台，
  后按 ADR-038 收敛为 macOS；macOS CI 已运行。

### 安全 / 隐私
- 凭据只进不出：不落盘、不打日志、不回传；`Debug` 被手工脱敏。
- fixture 脱敏脚本自带断言（原始长数字串 ∩ 输出 = ∅、不同 id 不得洗成同一假值、
  原始用户名与正文不得残留）。**fixture 只含脱敏后的值**。

### 已知限制
- `crawl.run` 是"跑到停为止再返回"（受 `max_pages` 约束）；长爬取请用小页数反复调用。
- `dl.events` 用游标轮询取增量，不是推送流（三种传输行为一致，见 ADR-029）。
- 只支持 **Aria2Next**，不支持上游 aria2（选项集与行为不同，见 `NOTICE`）。
- 爬取候选里的 `size_hint` 无值时省略（GraphQL 的 media 对象不带大小，
  而爬取阶段逐个探测会白白多出 N 次请求）。**真实大小由下载队列在下载前探测**，
  所以媒体下载的完整性校验照常成立。
- Windows / Linux 的 CI 尚未实跑验证（本机只有 macOS）。

### 许可证
- 本仓库 **GPL-3.0-only**；第三方组件的出处与义务见 [`NOTICE`](NOTICE)。
- 随包分发 Aria2Next 时必须附 `LICENSE` 与 `ARIA2NEXT-NOTICE.txt`（`script/package.sh` 会自动生成）。
