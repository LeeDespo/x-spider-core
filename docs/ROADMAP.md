# 路线图

> 里程碑定义与验收标准以 `docs/05-WORKFLOW.md` §2 为准。本文件只描述**现在**：
> 当前状态、未完成事项、平台计划与风险台账。M0–M4 与参考实现审计的**历史验收快照**
> （逐条证据、交付物清单、当时的风险）见
> [`history/milestones-2026-10.md`](history/milestones-2026-10.md)；
> 完整的版本级变化见 [`CHANGELOG.md`](../CHANGELOG.md)。

## 当前状态

| 里程碑 | 状态 |
|---|---|
| M0 地基 | ✅ 已完成（2026-10-01） |
| M1 取数 | ✅ 已完成（2026-10-01） |
| M2 爬取与下载 | ✅ 已完成（历史基线 2026-10-01；1.5.1 / 1.5.2 的增量修复见 CHANGELOG） |
| M3 分发 | ✅ 已完成（macOS 口径，ADR-038 / ADR-042） |
| M4 真实消费方 | ✅ 已完成（`xspider-cli` + `x-spider-mac` 接入） |
| 参考实现审计 | ✅ 已完成（ADR-033 / 034 / 035） |
| 安卓复用与审阅修复 | ✅ 已实施（2026-10-05；运行验收边界见 `docs/10` §8 与 `docs/RELEASING.md` §3） |

## 当前未完成

- **fixture 覆盖度**补齐到"每端点 5 类"：缺**空页（到底）**、**429**、**字段改名**三类
  （前两类可安全地造；第三类只能等 X 改版时由 canary 自动落盘）。
- **主动限流**：把 `x-rate-limit-*` 响应头接进限流状态（**主动**限流，而不只等 429）；
  fixture 里已留档。
- **`net.set_limits`** 的并发上限仍是启动时值，运行中调整未实现。
- **`host` 逃生舱**：早期架构只登记了名称；method、字段与宿主执行模型尚未设计
  （不属于当前 26 个 method，也不是已承诺事项）。
- **日期筛选**：跨月 / 跨年边界未测；"用户本地时区"这一层目前由外壳给定日历日期保证
  （ADR 见 `docs/CONTRACT.md` §4.11）。
- **Android 运行验收**：X 账号 GraphQL live、x86_64 设备、API 23 实机、真实 16 KB 页设备、
  前台服务 / Doze、MediaStore、Aria2Next Android 后端、用户 CA——清单与证据见
  [`docs/10-ANDROID-INTEGRATION.md`](10-ANDROID-INTEGRATION.md) §8，发布口径见
  [`docs/RELEASING.md`](RELEASING.md) §3。

## 平台计划

- **Windows / Linux**：不构建、不宣称（ADR-038）。出现真实消费方时：先加该平台的 CI
  构建与质量门，再加代码；发布口径见 `docs/RELEASING.md`。
- **Android**：构建与 API 36 模拟器冒烟已通过；推进方向是上表「Android 运行验收」清单，
  按 `docs/10` §8 逐项补运行证据，不以构建通过代替运行验收。

## 风险台账

| 风险 | 等级 | 说明与缓解 |
|---|---|---|
| X 改版（queryId / features / 字段 / 用户结构第三种形态） | **高** | 这是常态而非意外。缓解：fixture 回放测试 + live canary（改版当天红）+ 错误带字段上下文 + `parse` 错误码专用于"该报警了"；解析用"候选路径"读字段 |
| 账号限流（429 风暴） | **高** | 单一闸门 + 按配额域分桶 + 429 不重试 + 冷却到期自然失效（没有任何"需要人工清除"的状态） |
| 契约漂移（文档与实现不一致） | 中 | 契约守卫测试：schema 的 method 集合 ⟷ 代码集合相等；错误字段与 DTO 字段都在测试里核对 |
| 凭据泄露 | 中 | 凭据只进不出：`Debug` 手工脱敏、访问器 `pub(crate)`、契约不回显、fixture 脱敏脚本带自检 |
| 代理 / 网络不稳定（实测已发生） | 中 | 代理可运行时切换（ADR-007）；进程内无粘性网络状态；下载路径的代理逐任务显式传（`docs/12` 坑 33 / 34） |
| 阻塞 IO 出现在 async 上下文 | 中 | `spawn_blocking` 已用；clippy 盯不住，靠 code review |
| fixture 覆盖度"看起来够了"但没测到过滤逻辑 | 中 | `replay_offline.rs` 用**独立于实现**的遍历在 fixture 里找广告条目做交叉核对，"本次没测到"时明确打印 |
| 本地环境工具链被打断安装 | 低 | ADR-011：不写死版本号 |

## 下一阶段

无正在进行的里程碑。下一个里程碑立项时，先在 `docs/05-WORKFLOW.md` §2 定义验收标准，
再回填本文件的「当前状态」与「当前未完成」。

## 台账

- **2026-10-05 安卓复用与审阅修复（已实施）**：ADR-042 增量补齐 Android 专用 TLS、NDK 构建
  与 sidecar 打包脚本及 CI 构建检查，修复记录恢复与游标省略；macOS 离线质量门、双形态 / CLI
  smoke 与 release 打包签名检查全绿，Android 构建打包与模拟器冒烟通过、运行验收边界未变。
  详情见 [`history/milestones-2026-10.md`](history/milestones-2026-10.md)。
- **2026-10-06 文档体系整理（已实施）**：AGENTS.md 纳入版本控制并瘦身为路由入口；踩坑 45 条
  按主题迁入 docs/02/03/04/05/12（docs/05 §8 总索引）；docs/11 与 mac 接入实录迁 docs/history/；
  docs/06/08 改为消费端中立口径；新增发布真源 docs/RELEASING.md（当时名为 release.md）与 CI
  文档护栏。
- **2026-10-07 文档收敛（已实施）**：README 对齐 HelperNext 信息架构并新增 docs/README.md
  文档门户；发布真源更名 docs/RELEASING.md 并全仓换引用；本文件的历史快照迁
  history/milestones-2026-10.md 后瘦身为「只描述现在」；CHANGELOG 收敛为只记消费方可观察变化；
  docs_guard 增加废弃路径扫描与全仓 Markdown 链接检查。
