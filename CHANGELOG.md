# CHANGELOG

本文件记录**对外可见**的变化：契约、行为、产物。内部重构不写在这里。

格式参照 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)；
版本遵循[语义化版本](https://semver.org/lang/zh-CN/)。
契约版本的兼容规则见 [`docs/CONTRACT.md`](docs/CONTRACT.md) §6：
**method 与字段只增不改不删**。

## [未发布]

### 新增
- **`xspider-cli`**（`bins/xspider-cli`）：M4 的"第一个真实消费方"。
  它**不依赖任何 `xspider-*` crate**（守卫测试强制读它自己的 `Cargo.toml`），
  只经契约的本地 JSON-RPC 使用组件，因此它是"用别的语言写的外壳"的替身——
  契约缺什么、哪里别扭，它第一个炸。跑通的就是 M4 验收那条链路：
  取一页 → 按 `net.probe_size` 看体积 → 下 N 个媒体 → 报告结果。
  支持 `--dry-run`（离线 fixture 可跑，CI 用）、`--json`（机器可读报告）、
  `--list-methods`（外壳的启动自检）；退出码 0 成功 / 1 有媒体没下成 / 2 用法错 / 3 契约失败。

### 契约
- **契约版本 1.0.0 → 1.1.0**：只增 method，不改不删（`docs/CONTRACT.md` §6 的兼容规则）。
- **新增 method `net.probe_size`**：`{url} → {size}`。问 CDN"这个文件多大"，
  一次调用最多产生一个字节的流量；`size: null` 表示服务端没说（回放模式下恒为 `null`）。
  用途是让外壳在**决定下不下之前**按体积过滤（`docs/DECISIONS.md` ADR-033）。
  这与 `dl.enqueue` 内部的自动探测（ADR-032）不冲突，后者仍是缺省行为。
- **`dl.events` 的事件形状进了 schema**：新增 `$defs/downloadEvent`（`oneOf` 四种，
  各自 `additionalProperties: false`）与 `$defs/integrity`，`items` 指向它，
  并有契约守卫测试盯字段集合。此前机器可读契约在这里写的是
  `{"items": {"type": "object"}}`——等于把"猜"留给每个消费方。

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

### 计划中
- `fetch.mutate`（点赞 / 转推 / 书签）：写操作的风险边界待确认。
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
    重启对账续传、引擎选择（留在组件内部）；
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
- CI：三平台 `fmt + clippy -D warnings + test`，macOS/Linux 额外做 cdylib 的 dlopen 验证。

### 安全 / 隐私
- 凭据只进不出：不落盘、不打日志、不回传；`Debug` 被手工脱敏。
- fixture 脱敏脚本自带断言（原始长数字串 ∩ 输出 = ∅、不同 id 不得洗成同一假值、
  原始用户名与正文不得残留）。**fixture 只含脱敏后的值**。

### 已知限制
- `crawl.run` 是"跑到停为止再返回"（受 `max_pages` 约束）；长爬取请用小页数反复调用。
- `dl.events` 用游标轮询取增量，不是推送流（三种传输行为一致，见 ADR-029）。
- 只支持 **Aria2Next**，不支持上游 aria2（选项集与行为不同，见 `NOTICE`）。
- 爬取候选里的 `size_hint` 恒为 `null`（GraphQL 的 media 对象不带大小，
  而爬取阶段逐个探测会白白多出 N 次请求）。**真实大小由下载队列在下载前探测**，
  所以媒体下载的完整性校验照常成立。
- Windows / Linux 的 CI 尚未实跑验证（本机只有 macOS）。

### 许可证
- 本仓库 **GPL-3.0-only**；第三方组件的出处与义务见 [`NOTICE`](NOTICE)。
- 随包分发 Aria2Next 时必须附 `LICENSE` 与 `ARIA2NEXT-NOTICE.txt`（`script/package.sh` 会自动生成）。
