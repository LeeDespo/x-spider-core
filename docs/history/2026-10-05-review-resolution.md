# 11 · 2026-10-05 审阅发现归并与处置

> 本文记录审阅发现的修复位置与验收边界。输入为 `发现清单-2026-10-05.md` 与配套 Android 复用报告。
> 报告标题中的 88 是审阅记录数，不是 88 个独立代码缺陷：文档问题有同因多处命中，Android 运行风险也由多个视角重复发现。本表按根因归并，并保留未验收边界。

## 文档审阅：53 条发现按章节归并

| 报告章节 | 记录数 | 处置状态与证据位置 |
|---|---:|---|
| 契约一致性 | 7 | 已归并处理。schema 标题同步到 1.5.2；`crawl.run` 的 `dropped`、带序号事件、`stop_when_older_than` 与 candidate 形状现由 schema/CONTRACT 明确；耗尽时 `next_cursor` 省略；`dl.plan` / `dl.report` 仅是未设计的预留名称。索引链接已锚定到 `docs/07` 稳定章节锚点。具体形状见 schema、CONTRACT 及 ADR-042。 |
| 接口参考与接入 | 8 | 已修 `docs/06`–`docs/08`：`subscribe()` 仅 Rust 内部；轮询是对外事件方式；补 `auth_required`、stdio 握手差异和取消/恢复边界；更正 `docs/06` 列表锚点；`dl.plan/report` 不再宣称已有字段形状；重启对账与已决事项状态更新。 |
| 架构与领域知识 | 9 | 已修 `docs/01`–`docs/02`：架构加入 `posts[]`、轮询事件和契约错误码；`host` 不再作为已有后端介绍；E6–E9 归回下载章节并纠正 Aria2Next 已退役参数；行为参照标为 `x-spider-mac` 接入前历史/备份；关闭过时 G3 建议。 |
| 工程纪律 | 9 | 已修 `docs/03`–`docs/05`：跨平台范围按 ADR-038/042 重写；说明打包归档同时包含 sidecar 与 cdylib，二进制名为 `aria2next`，仓库不 vendor；修正 ADR 示例；录制、fixture 目录、canary 覆盖、脱敏不变量及豁免按脚本和真实目录更新。 |
| 状态台账 | 12 | 已修 README、CHANGELOG、ROADMAP、DECISIONS 与 `fixtures/README.md`：区分历史基线与当前 1.5.2；标注 `quoted` / 外壳恢复和对账缺口已闭合；更新 fixture 匹配游标 +8；ADR-011 保持 stable 并更新推翻条件；ADR-031 标为受 ADR-038 收窄；新增 ADR-043（null/省略）与 ADR-044（fixture 匹配打分）；补 `docs/09` / `docs/10` 入口。 |
| 本地操作手册 | 8 | 已修根目录 `AGENTS.md`：公开文档范围更新至 01–10、加入 09/10 阅读顺序、移除 M1/CI/接入缺口旧状态、去掉过时测试数量、改为接入分支已并回主线后的参照纪律。`docs/00-KICKOFF.md` 已有历史简报提示；`ACCEPTANCE.md` 未改，仅新增 `.gitignore` 规则。 |

以上为归并后的文档处置，不表示所有同名实现风险均已实机验证。schema、CONTRACT 与实现由契约守卫测试核对；其余文档按上述路径追踪。

## Android 复用审阅：重复风险归并为 12 组

| 风险组 | 处置 | 当前边界 / 责任 |
|---|---|---|
| Android target、NDK linker、构建与打包缺位 | **构建链路已实现**：独立 `script/android-build.sh`、package 路径与 CI 构建检查已添加；方法与产物说明见 `docs/03` §6、`docs/10`。 | 构建通过只证明编译/链接及打包检查；Android 普通 UID 模拟器证据见 `docs/10` §8。 |
| TLS 信任根 | **实现已按平台隔离**：Android 目标用 webpki 公共根；macOS 原生信任根不变。 | 不据此声称用户自装 CA、特定代理链或所有设备环境已测试。 |
| Android sidecar 部署 / W^X / nativeLibraryDir | **决策为 sidecar 主形态**，沿用现有 26 methods 与 3 个 C ABI，不新增 configure/cancel 接口；部署约束与 APK 集成见 `docs/10`。 | `nativeLibraryDir` / 普通 UID 启动验证见 `docs/10` §8；JNI cdylib 是受限备选，不是本轮交付路径。 |
| 后台进程与 Android 12+ 子进程回收 | **按可恢复设计处理**：持久化任务状态，外壳前台服务维持工作并在回前台用 `dl.list()` 对账。 | 系统杀进程不能被组件保证消除；Doze、后台限制和恢复行为仍是消费者设备验收项。 |
| 下载状态落盘、取消与并发锁 | **实现按 ADR-042**：version 1 记录可读；Waiting/Active 恢复为 Waiting，Paused 保持，Error（含取消）不自动重试；取消清理期间暂拒 resume。Unix 用内核 `flock`，环境 state-dir 同样取锁，锁文件保留 inode。 | `dl.prune` 维持只清内存快照的既有语义。快速 pause/cancel/resume 的验证记录见 `docs/10` §8。 |
| Aria2Next Android 可执行文件与默认下载目录 | `XSPIDER_ARIA2_DIR` 可覆盖默认临时下载目录，默认行为不变；Aria2Next 仍是可选外派后端。`LICENSE.aria2` 与随包声明已补。 | Android 预编译 Aria2Next 的 APK 启动、下载 E2E 尚未验收；Android 主路径可用内置 HTTP。 |
| FFI 同步调用、取消与配置入口 | **本轮保持三函数 C ABI**；Android 推荐 sidecar，HTTP 调用已有并发与取消能力。 | 不在契约中加入 JNI 类型；若未来真实消费者证明 sidecar 不满足生命周期，另立 ADR 设计增量接口。 |
| cleartext localhost、API 版本差异 | Android 接入边界见 `docs/10`，按 API 28–36 与 API 37+ 的 loopback 行为分别说明。 | 外壳需按自身 target/API 配置与传输形态验证；此文档说明不代替目标设备验收。 |
| 服务保活、事件丢弃、回前台对账 | 由外壳负责前台服务、生命周期与 `dl.list()` 对账；事件使用轮询，旧事件可能按环形缓冲规则丢弃。 | 产品必须将权威状态同步建立在 `dl.list()`，不能只依赖事件回放。 |
| 文件目录、分区存储、MediaStore | `dest_dir` / `file_name` 与产品保存路径仍由外壳决定；路径交给组件后应保持稳定。 | 应用私有目录、MediaStore 注册、用户选择目录和移动文件策略均属外壳验收。 |
| 凭据与代理来源 | 凭据仍只由外壳注入；代理由外壳选择后经 `net.set_proxy` 传入。 | Android CookieManager/安全存储、不进入 logcat，以及系统代理/VPN/应用内代理策略由外壳负责。 |
| `host`、同步更新与平台范围 | `host` 只是架构预留名；组件升级版本握手见 `docs/10`；README/ROADMAP 已说明 Android 构建状态。 | 未设计 `dl.plan/report`，不宣称 Windows/Linux 或 Android 运行时已全面支持。 |

## 误报与仍未验收

报告早期将一次 Android `cargo check` 的 E0463 描述为“核心代码编译不过”。复核后发现运行命令调用了 PATH 中的 Homebrew `rustc`，其 sysroot 缺目标标准库；rustup 工具链目标已安装，错误发生在编译到本仓库代码之前。这是**工具链选择造成的环境误报**，不能用作产品代码失败证据，也不能反向证明设备运行成功。修正记录在根目录 `AGENTS.md` 踩坑 43；现行 NDK 路径见 `script/android-build.sh` 和 `docs/10`。

最终源码已通过独立审阅，审查发现的过期派发、目标文件误删和取消清理失败后无法重试三处风险已修复并补回归用例。macOS 全仓离线 test、clippy、fmt、双形态/CLI smoke 与 release 签名检查通过；Android 两 ABI 实际构建打包、arm64 API 36 普通 UID 的 HTTP/stdio/C ABI、本地下载和公开 TLS 探测通过。具体环境与命令见 `docs/10` §8。

仍未取得 X 账号 GraphQL live、真实 16 KB 设备、x86_64 设备、Aria2Next Android APK、Doze/phantom process、MediaStore 或用户 CA 验收证据；Android CI 本轮未实跑。这些是明确的验收边界，不以编译成功替代。
