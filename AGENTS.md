# AGENTS.md — X-Spider Core

本文件是 AI agent 的**常驻指令**与仓库开发规范入口，每轮开始前先读。

> **本文件自 2026-10-06 起纳入版本控制、对外可见。**
> 旧版「本文件不上传 GitHub（连同启动简报），README 不要链接本文件」的说明**作废**——
> 现在 README 可以放心链接本文件。

本文件只做**路由**：划定边界、指路真源、列收尾检查。事实与细节都在各主题文档里，
不要往本文件里堆进度叙述、命令手册或长篇坑单。原「踩坑记录」（45 条，每条都是一次
真实返工）已按主题迁入 `docs/02` / `docs/03` / `docs/04` / `docs/05` / `docs/12`
与 `docs/history/`，总索引见 `docs/05` §8；原「常用命令」整块迁入 `docs/05` §9。

本文件是**跨工具唯一**的项目级规范：不要为单个 agent 工具另建近似副本；
工具私有、机器私有的配置留在本地、不入库。

---

## 这是什么项目

用 Rust 实现两个可复用组件，让不同平台的「外壳应用」不必再各自实现 X（Twitter）的数据获取与下载：

| 组件 | 职责 |
|---|---|
| `xspider-fetch` | 在线数据获取：用户、媒体时间线、推文时间线、推文详情树、搜索、主页时间线、关注列表、互动（点赞/转推/书签） |
| `xspider-download` | 爬取调度 + 媒体下载：翻页、筛选、队列、并发、断点续传、完整性校验、暂停/恢复/取消、事件上报 |
| `xspider-core`（**不对外发布**） | 共享内核：HTTP 客户端、凭据注入、限流闸门与 429 熔断、请求签名、错误分类、DTO 序列化。两个组件共用，**必须抽出来**——否则限流会被各实现一遍，而 429 治理是这条链上最容易出事的地方 |

**交付形态两套，共用同一份核心逻辑，行为必须一致**：

1. **sidecar 可执行文件 + 本地 JSON-RPC（主形态）**——换组件 = 换一个二进制；
2. **cdylib（次形态）**——给需要进程内调用的外壳。

预期消费方：`x-spider-mac`（macOS SwiftUI，已接入），以及将来可能出现的 Windows / Linux 外壳。

`bins/xspider-cli` 是仓库自带的**真实消费方**：它不链接任何 `xspider-*` crate
（守卫测试强制），只经契约的 JSON-RPC 驱动组件——契约缺什么、哪里别扭，
它第一个炸（见 `docs/06` §1）。

本仓库是**组件，不是产品**：UI、文件名模板、用户目录选择等有意留在外壳——
分工清单见 `docs/08` §4。

---

## 真源（Sources of Truth）

| 问题 | 真源在哪 |
|---|---|
| 对外行为（契约） | `docs/CONTRACT.md` + `contract/xspider.schema.json`（同一契约的两种表示，必须同步改）+ 契约测试 |
| 接口形状（入参 / 出参 / 时序） | `docs/07-API-REFERENCE.md` + `docs/09-METHOD-INDEX.md` |
| X 上游行为 | `fixtures/`（真实响应）+ `docs/02-X-DOMAIN-NOTES.md` + 实现测试 |
| 能力与实现位置 | `docs/08-CAPABILITY-MAP.md` |
| 测试与 fixture 纪律 | `docs/04-TESTING-AND-FIXTURES.md` + `fixtures/README.md` |
| 架构与决策 | `docs/01-ARCHITECTURE.md` + `docs/DECISIONS.md`（ADR） |
| 进度与风险 | `docs/ROADMAP.md` |
| 发布 / 打包 / 分发 | `docs/release.md` |
| 历史（迁移 / 审阅 / 接入实录） | `docs/history/`——**默认不读**，回归调查、考古、追历史决策才查；与活文档冲突时以活文档为准 |

**不要当真源的东西**：某个具体外壳的当前实现（它是消费者，不是协议）；
`docs/history/` 里的历史记录（它是当时的证据，不是现在的要求）；
本文档以外的任何进度叙述。

**文档纪律**：同一件事实只在一个地方维护——新事实写进拥有它的主题文档，别处只留路由；
历史材料进 `docs/history/`，不进活文档。

---

## 读文档的路由

不必每轮全读：先读本文件定边界，再按任务挑 1–3 份主题文档——改哪一层，读哪一份。

| 顺序 | 文件 | 你会得到什么 |
|---|---|---|
| 0 | `docs/CONTRACT.md` + `contract/xspider.schema.json` | **对外契约本身**。改代码前先看它 |
| 1 | `docs/01-ARCHITECTURE.md` | 组件边界怎么切、契约长什么样、下载引擎三种后端、任务归属 |
| 2 | `docs/02-X-DOMAIN-NOTES.md` | X GraphQL 的领域知识与**踩过的坑**（照着做能省几周） |
| 3 | `docs/03-FFI-SIGNING-PACKAGING.md` | FFI / 签名 / 分发 / 交叉编译的实测结论 |
| 4 | `docs/04-TESTING-AND-FIXTURES.md` | 测试与 fixture 纪律、离线默认、live canary |
| 5 | `docs/05-WORKFLOW.md` | 里程碑、ADR 纪律、汇报格式、工程陷阱、**踩坑总索引（§8）与命令手册（§9）** |
| 6 | `docs/DECISIONS.md` | 已做的决定与**每条什么时候该被推翻** |
| 7 | `docs/ROADMAP.md` | 实际进度、待办清单、风险台账 |
| 8 | `fixtures/README.md` | fixture 怎么产生、覆盖度、**哪些缺口是有意留的** |
| 9 | `docs/06-CONSUMER-INTEGRATION.md` | **接入手册**：接入要动哪些代码、消费者视角踩到的坑、未决清单 |
| 10 | `docs/07-API-REFERENCE.md` | **接口参考**：26 个 method 的入参/出参、数据形状、错误处理、可直接抄的时序（写外壳时最常翻的一份） |
| 11 | `docs/08-CAPABILITY-MAP.md` | **能力地图**：每个契约 method 在哪个 crate 实现、怎么测、当前缺口、有意不进组件的事 |
| 12 | `docs/09-METHOD-INDEX.md` | **接口索引**：26 个 method 直达 `docs/07` 的详细章节 |
| 13 | `docs/10-ANDROID-INTEGRATION.md` | Android 构建、部署、外壳义务与未完成的运行验收 |
| 14 | `docs/12-RUNTIME-NOTES.md` | **运行时注记**：下载队列状态机、断点与 epoch、sidecar 生命周期与看门狗 |
| 15 | `docs/release.md` | **发布 / 打包 / 分发必读**：版本基线、tag 纪律、资产清单、CI 发布流程 |
| 16 | `docs/history/` | 历史归档（原审阅记录与 mac 接入专项）：**仅回归调查 / 考古时查** |

按任务找入口（「先读」指上面表的序号）：

| 任务 | 先读 |
|---|---|
| 改契约 / 加减 method | 0 + 10 + 4；method 集变化再加 12 |
| 改取数 / 解析 / X 协议 | 2 + 8 |
| 改下载 / 队列 / sidecar | 14 + 4 |
| 改构建 / FFI / 打包 | 3 + 15 |
| 发版 | 15 + 7 |
| 接入新外壳 | 9 + 10 + 12 |
| 加测试 / 查测试纪律 | 4 + 8 |
| 改 Android 构建 / 部署 | 13 + 3 |
| 只查接口细节 | 12 进 10 |

`docs/CONTRACT.md`、`docs/DECISIONS.md`、`docs/ROADMAP.md` 由你创建并持续维护。

---

## 铁律（违反即返工）

1. **契约优先**：先改 `docs/CONTRACT.md`，再改代码。契约的任何变更必须在**同一次变更**里同步：
   - `docs/CONTRACT.md` 与 `contract/xspider.schema.json`；
   - `docs/07-API-REFERENCE.md`；method 集变化时加 `docs/09-METHOD-INDEX.md`；
   - `CHANGELOG.md` 与契约测试；
   - 破坏性变更写 ADR（进 `docs/DECISIONS.md`）。
2. **契约必须语言中立**：对外只有 `xspider_call(method, json) -> json` 一个入口，加 `xspider_version()` 做版本握手。**不许**把 Rust 类型、绑定生成器类型、queryId、`features` 常量、端点路径、HTTP 头暴露进契约。
3. **默认离线测试**：`cargo test` 不碰网络。真网络测试必须 `XSPIDER_LIVE=1` 门控。
4. **没有真实响应 fixture，就不许声称"功能完成"**。
5. **凭据只进不出**：cookie / token 不落盘、不打日志、不回传。凭据由外壳注入。
6. **限流是一等公民**：任何新增请求路径都必须过共享内核的闸门；GraphQL API 与媒体 CDN 的配额分开治理。
7. **踩过的坑当天写下来**（现象 → 根因 → 解法），写进对应主题文档——`docs/02` 协议 /
   `docs/03` 构建 / `docs/04` 测试 / `docs/05` 工作流 / `docs/12` 运行时 /
   `docs/history/` mac 专项——并在 `docs/05` §8 的踩坑总索引登记一行。
8. **消费端是消费者，不是协议真源**：`x-spider-mac` 已接入本组件。仅迁移回归 / parity
   审计时读它的 `pre-component-integration` 历史；不要把它现在的 `TwitterAPI`
   映射层误认成取数实现。除非任务明确要求，不在该仓库追加改动。

---

## 禁止做的事

- 做任何 UI。
- 在 README 或文档里宣传「绕过 X 限流」；定位写成个人自用、授权账号范围内的工具。
- 依赖 Apple 专有框架（Keychain / Security.framework）——那就没法跨端了。
- 一次性铺开所有端点。**先垂直切片（3 个端点端到端 + 双形态 + 契约测试），再横向复制。**
- 用「错误文案字符串匹配」做任何逻辑判断（重试、分类、跳过）。
- 把组件 A 与组件 B 直接对接（见 `01-ARCHITECTURE.md`：两者通过外壳的「候选清单」衔接）。

---

## 参考实现的历史地位

本仓库是 `x-spider-mac` 的抽取；但如今**对外行为的真源已是契约 + 契约测试 + 真实 fixture**，
不再是某个具体外壳。
`x-spider-mac` 只在两类场景有用：迁移回归，以及「请求长什么样」的考古——那时看它的
`pre-component-integration` tag，不要拿它当前经组件的 `TwitterAPI` 映射层当
「该怎么取数」的答案。
对齐的永远是**可观测行为**（字段值、请求头、传给 aria2 的选项），不是代码结构——
组件有自己的分层（`docs/01-ARCHITECTURE.md`），不要为了「像」而把边界糊掉。
确实要偏离参考实现时（例如实测证明它的做法是错的），**写 ADR 说明「为什么不一样」**。

---

## 许可证与合规

- 本仓库 **GPL-3.0-only**：解析与分页逻辑源自 GPL-3.0 的 `MiningCattiva/x-spider` 移植，衍生作品须沿用，并在 README 注明出处。
- aria2-next 是 **GPL-2.0** 的独立程序：通过**子进程 + JSON-RPC**使用属于「聚合」，不传染本仓库；随包分发的具体义务见 `NOTICE` 与 `docs/release.md`。
- 不提供任何面向公众的抓取服务，不发布抓取结果数据集。

---

## 当前进度

进度与风险台账：`docs/ROADMAP.md`（完成里程碑、闭合缺口或发现新风险后更新它）。
本文件不保留任何进度叙述；里程碑定义与验收标准在 `docs/05` §2，
版本基线与发布状态见 `docs/release.md`。

---

## 质量门（提交前必须三条全绿）

> PATH 里排在前面的是 Homebrew 的 cargo，它是真实二进制、**不读 `rust-toolchain.toml`**；
> 要用仓库锁定的工具链，显式用 `~/.cargo/bin/cargo`（rustup 垫片）。
> 实测背景见 `docs/03` §8 踩坑 1。

```bash
CARGO=~/.cargo/bin/cargo

$CARGO test --workspace --offline                                  # 默认离线，不碰网络（铁律 3）
$CARGO clippy --workspace --all-targets --offline -- -D warnings
$CARGO fmt --all --check
```

`--offline` 让 cargo 只用本地缓存；首次构建或改了依赖才需要联网 `cargo fetch`。
按改动范围加跑 `docs/05` §9 里的对应命令（下载 / 队列改动跑下载 E2E，
跨形态改动跑 `script/smoke.sh`；live 与录制命令也在那里）。
完整命令手册（下载 E2E / smoke / CLI / live / 录制 / 脱敏 / sidecar 调试）见 `docs/05` §9。

---

## 完成检查（每轮收尾自查）

- **契约变了** → 逐项过铁律 1 的同步清单（CONTRACT / schema / `docs/07` /
  `docs/09` / CHANGELOG / 契约测试 / 破坏性变更的 ADR）。
- **普通修复** → 补测试 + 更新相关的「当前事实」文档——哪个文档的真值变了就改哪个；
  进度变化写 `docs/ROADMAP.md`。
- **新坑** → 当天写进对应主题文档（铁律 7 的分工），并在 `docs/05` §8 总索引登记。
- **动了 live / 真实账号** → 最小副作用、操作可逆、凭据不落盘不打日志
  （铁律 3 与 5；live 纪律见 `docs/04` §6）。
- **提交前** → 三条质量门全绿。
- 汇报格式（五段，不要流水账）见 `docs/05` §5。
