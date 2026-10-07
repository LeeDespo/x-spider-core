# 03 · FFI、签名、分发与交叉编译（**含实测结论**）

> 这里的历史「实测」结论来自 macOS，不能外推为其他平台验收。
> Android 构建与外壳接入边界见 [`10-ANDROID-INTEGRATION.md`](10-ANDROID-INTEGRATION.md)；
> Android 真机、Aria2Next 与 live 结果按本轮验收记录另行更新。
> 标「推断」的只有推理链，未经实测——用之前先自己验一遍。

---

## 1. 为什么主形态是 sidecar 而不是 dylib

**历史实测环境**：ad-hoc 签名、未启用 hardened runtime 的 macOS 外壳产物；当时使用 `x-spider-mac` 做宿主验证。结论已固化为本仓库的签名 / 打包测试，日常开发不依赖该消费端历史源码。

| 场景 | 结果 | 结论 |
|---|---|---|
| 复制 App、替换 `Contents/Resources/aria2next` | `codesign --verify` 报 `a sealed resource is missing or invalid`，但 `open` **成功**、进程正常运行 | 换资源文件**不影响启动** |
| 替换成**未签名** Mach-O 并执行 | 被内核 **SIGKILL（退出码 137）** | arm64 要求每个可执行代码文件**自己**有有效签名（ad-hoc 即可） |
| 替换成**从网上下载**的二进制（带 quarantine） | 同样 **137** | 要先 `xattr -dr com.apple.quarantine` |
| 普通 host（无 hardened runtime）`dlopen` ad-hoc dylib | **成功** | 换组件 dylib 在**当前分发方式下可行** |
| 换另一个编译批次的 ad-hoc dylib | **成功** | 只要契约不变，重新构建也能换 |
| `dlopen` **未签名** dylib | 失败：`missing code signature in <...>` | 组件文件必须签名 |
| **hardened runtime** 的 host（ad-hoc，无 Team ID）`dlopen` ad-hoc dylib | 失败：`mapping process and mapped file (non-platform) have different Team IDs` | **一旦公证/hardened runtime，"换 dylib"就死路一条**——没有 Team ID，永远满足不了"同一 Team ID" |
| 同上，但给 host 加 `com.apple.security.cs.disable-library-validation` | **成功** | 这是唯一救济，代价是 Apple 会对该程序做额外安全审查 |

**签名身份**：对比改动前后的 Designated Requirement，都是
`cdhash H"4d3732…" or cdhash H"eb6b27…"`（universal 的两切片各一个哈希），**完全没变**。
→ 推断：替换资源文件不改变应用身份，**TCC 权限（文件夹访问、通知）大概率不受影响**；
→ 推断：**若重新编译并替换主可执行文件，cdhash 变化，系统会当成另一个应用，权限需重新授权**。

**macOS 结论**：sidecar 形态下两个进程各自签名（ad-hoc 即可）、互不验证、不涉及 library validation、
也不破坏 bundle 密封。**这是 macOS 选它当主形态的实证理由**；平台主次见 ADR-042，不能据此推断 Android 生命周期表现。

---

## 2. 契约与绑定的纪律

- 对外只有 `xspider_version()` / `xspider_call(method, json)` / `xspider_free(ptr)` 三个 C ABI 函数
  ——C ABI 是唯一跨编译器、跨语言稳定的接口面。
- **不用生成的强类型绑定定义契约**（BoltFFI / UniFFI 都算）；只把生成绑定当作**外壳侧的便利**。
- 用 BoltFFI 的话：**锁定精确版本**，并接受"绑定只在外壳侧重新生成、ABI 只允许加法式变更"。
- cdylib 的导出符号保持最小；**不要**导出 Rust 的泛型/结构体布局依赖的接口。

---

## 3. sidecar 的形态与生命周期

- 传输：**本地 HTTP JSON-RPC（`127.0.0.1`）为主**——与 aria2 一致、可用 `curl` 调试；
  `--stdio`（JSON Lines）作为备选。
- 启动协商：支持 `--port 0`，绑定随机端口后在 **stdout 打印一行**
  `ready {"port":N,"token":"...","version":"..."}`，外壳读取该行即完成握手。
- 鉴权：每次运行生成**随机 token**（请求头携带），即使本机其他进程也不该随便调用。
- 单实例锁 + 优雅退出：收到 `shutdown` method / SIGTERM 时收尾（结束子进程、落盘状态）。
- **多实例安全**：如果同时运行多个消费者（例如 mac App + CLI），必须支持
  **每实例独立端口 + 独立 state-dir / session 文件**，否则两个进程会互踩。
- 崩溃与重连：外壳要能用 `dl.list()` / `net.status` **重新对账**，而不是假设状态还在内存里。

---

## 4. aria2-next 的事实与分发

**实测/核查（2026-09-30）**：

| 项 | 值 |
|---|---|
| 仓库 | `AnInsomniacy/aria2-next`（Aria2Next 项目；版本与资产以其 Release 为准） |
| 版本 | 本仓库不维护“最新版本”；兼容性实测版本与实际分发版本分别见本节历史记录和发布包内说明 |
| 许可证 | **GPL-2.0** |
| 预编译资产 | `macos-arm64` / `macos-x86_64` / `linux-x86_64` / `linux-aarch64` / `windows-x86_64.exe` / `windows-arm64.exe` / `android-arm64` + `checksums.sha256` |
| 迁移期兼容性实测 | **2.7.5**（2026-10-01 历史记录；不是本仓库 vendor，也不是当前版本约束） |

**补充实测（2026-10-01，Aria2Next 2.7.5，arm64）**：

| 项 | 实测 |
|---|---|
| 产品标识 | `aria2.getVersion` → `product: "aria2-next"`、`version: "2.7.5"`、`rpcVersion: "1.1.0"` |
| 只支持 Aria2Next | 启动前用 `--version` 预检、启动后用 `product` 校验；指向上游 aria2 直接拒绝 |
| 选项集差异 | 上游 `--split` / `--max-connection-per-server` 在 Aria2Next fork 中已退役；应使用 fork 专有 `--stream-max-connections` |
| RPC 错误码 | **全都是 `code: 1`**（Unknown option / GID not found / Unauthorized 都一样）→ 不能按码分类 |
| 404 行为 | 报 `status=complete` + 0 字节文件 → 完成判据必须自己校验落盘字节数 |
| 子进程随父退出 | `--stop-with-process=<pid>` 有效（实测父 shell 退出后引擎自己停了）；代码里另有 `kill_on_drop` 兜底 |

**关键判断**：aria2 的跨端统一性在**协议**（JSON-RPC + 选项语义），不在二进制；
二进制按平台各一份，**上游已经交叉编译好了**。所以组件不吸纳它的**代码**，
只吸纳它的**启动方式 + RPC 协议 + 引擎能力抽象**（进程外调用属"聚合"，不传染许可证）。

**二进制从哪来（三选一）**：

| 方案 | 说明 | 评价 |
|---|---|---|
| **A. 随发行包携带**（`xspiderd` + `aria2next` + NOTICE/LICENSE） | 外壳零配置 | 可选；打包脚本只在显式提供路径时携带 |
| **B. 外壳提供路径**（`XSPIDER_ARIA2_PATH`） | 组件保持纯 Rust 依赖 | 当前外壳接入采用此路径；组件默认仅用内置 HTTP 后端 |
| **C. 组件运行时自动下载** | 按平台拉二进制 | 当前不实现；每个平台的签名、来源与许可义务都需单独设计 |

**签名要求（打包脚本的事）**：macOS 二进制需 **ad-hoc 签名**（arm64 强制）；
Windows 需 **Authenticode** 才不弹 SmartScreen；Linux 无要求。
**引擎版本显式化**：把"用哪个二进制、什么版本、校验和"变成配置 + 启动时 `--version` 校验，
外壳提供或随包携带时均明确版本与校验和；本仓库不 vendoring Aria2Next 二进制。

---

## 5. 打包产物结构

```
dist/xspiderd-<ver>-<platform>-<arch>.tar.gz
├── xspiderd                 # sidecar 可执行文件（ad-hoc 签名）
├── aria2next                # 可选：随包携带（附校验和）
├── libxspider.dylib|.so|.dll# cdylib 与 sidecar 一起进入当前平台发行归档
├── xspider.schema.json      # 机器可读契约
├── LICENSE  NOTICE          # GPL-3.0-only + aria2-next 的 GPL-2.0 声明与源码链接
└── CHANGELOG.md
```

macOS 需要 universal 时分别编译两个架构后 `lipo`，并**对合并后的文件重新 ad-hoc 签名**。

macOS CI 运行 `script/smoke.sh`：
构建 → 起 sidecar → `curl` 调 `xspider_version` 与一个真实 method（fixture 或 live）→
断言字段 → 关停 → 检查无残留子进程（二进制名为 `aria2next`）。
Android 构建检查与打包入口见 §6 和 docs/10；它们不等于设备运行测试。

---

## 6. 交叉编译（与坑）

| 目标 | 工具 | 坑 |
|---|---|---|
| macOS arm64 / x86_64 | 本机编译 + `lipo` 合 universal | 合并后必须重新签名 |
| Linux x86_64 / aarch64 | `cargo-zigbuild` 或 `cross`（工具链参考，未承诺交付） | glibc 版本后缀有诸多 caveat；`crt-static` 不支持 |
| Windows x86_64 / arm64 | Windows 原生构建（工具链参考，未承诺交付） | `cargo-xwin` 需接受 MSVC 许可、要 clang，调试成本高 |
| Android aarch64 / x86_64 | NDK 27.3.13750724、API 23；两 ABI 实际链接打包通过 | arm64 API 36 普通应用 UID 的 HTTP/stdio/C ABI 与公开 TLS 验证通过；16 KB 仅对齐检查，完整边界见 docs/10 §8 与 ADR-042；交叉编译假信号与验收层次见 §8 坑 43 / 坑 45 |

**两个会拖死交叉编译的选型**（写在 `Cargo.toml` 之前就要定）：
1. **HTTP 栈用 `reqwest` + `rustls-tls`，不要 `native-tls`**——否则 Windows/Linux 交叉编译要处理 OpenSSL；
2. **避免依赖需要 C 工具链的 crate**；确实需要时优先选纯 Rust 实现。

**工具链接线（详见 §8 坑 1 / 坑 43）**：交叉编译报 `error[E0463]: can't find crate for 'core'`
未必是 target 没装——先确认 cargo 实际抓的是哪个 rustc（Homebrew 的 cargo / rustc 是真实二进制，
不读 `rust-toolchain.toml`、只带 host std）。Android 的 PATH 前置与 `CC_*` / `AR_*`
（NDK clang / llvm-ar）接线已固化在 `script/android-build.sh`，复跑证据见 `docs/10-ANDROID-INTEGRATION.md`。

**Windows 特有事项**：路径分隔符与大小写不敏感（去重/改名逻辑要小心）、
本地 socket/端口可能触发防火墙提示、DLL 加载不校验签名但需要 Authenticode 才不受 SmartScreen 干扰。

---

## 7. 兼容与降级（面向外壳）

- 启动握手失败（契约版本不匹配 / 组件文件未签名 / 被 quarantine）时，
  **拒绝启动并给出可操作的人话提示**，例如：
  `组件文件未签名或被隔离：请执行 xattr -dr com.apple.quarantine <路径>`
  ——不要让功能静默失灵（未签名二进制表现为 137 静默死亡，最难排查）。
- 启动自检项：架构、签名状态、契约版本、依赖的 aria2 二进制是否可用与版本是否匹配。

---

## §8 踩坑记录（2026-10-06 迁入自 AGENTS.md，保留原编号）

> 格式沿用 AGENTS.md「踩坑记录」：**现象 → 根因 → 解法（含证据）**。
> 以下 4 条是「工具链 / 打包」类，自根目录 `AGENTS.md` 迁入；编号与标题句保持原样，
> 正文保留全部实测证据（数字、路径、错误文本），只做轻度顺滑。
> 交叉编译的目标矩阵与平台注意事项见上文 §6；Android 的验收记录与「仍未验收」边界见
> [`10-ANDROID-INTEGRATION.md`](10-ANDROID-INTEGRATION.md) §8。

### 1. `rust-toolchain.toml` 写死具体版本会把工具链装成半成品。

- 现象：写 `channel = "1.98.1"` 后，rustup 首次构建报
  `error: could not remove 'component' file: .../manifest-rustfmt-preview-aarch64-apple-darwin`
  与 `could not rename 'component' file ... Directory not empty (os error 66)`，
  工具链处于"半装"状态；之后每次 `cargo` 调用都会先尝试同步 channel（明显变慢）。
- 根因：代理网络下下载中断，rustup 的组件安装不是原子的，回滚也失败了。
- 解法：写 `channel = "stable"`（本机实测 = `rustc 1.98.1 (48a229cea 2026-09-01)`），
  把实测版本写进文件注释；可复现性由 `Cargo.lock` + 注释共同保证。见 ADR-011
  （现状即 `rust-toolchain.toml` 的 `channel = "stable"` + 注释里的实测版本）。
- 顺带：**Homebrew 的 cargo 不读 `rust-toolchain.toml`**（它是真实二进制，不是 rustup 垫片），
  所以要用 `~/.cargo/bin/cargo`。这一条不写下来，下一轮 agent 会以为锁文件没生效。
  它与坑 43 同源：PATH 里排在前面的 Homebrew rustc 只带 host std，交叉编译时第一个撞上。

### 9. cdylib 的产物名默认取 crate 名。

- 现象：`crates/xspider-ffi` 产出 `libxspider_ffi.dylib`，而文档约定的是 `libxspider.dylib`（即 §5 产物树里的名字）。
- 解法：`[lib] name = "xspider"`，依赖方写 `use xspider::...`。打包时不必再改名。见 ADR-012。

### 43. 交叉编译时 cargo 经 PATH 找到的 rustc 不是 rustup 工具链的——"安卓编译不过"可能是假信号。

- 现象：`cargo check --target aarch64-linux-android` 报
  `error[E0463]: can't find crate for 'core'`（提示 target 未安装），
  但 `rustup target list --installed` 明明列着 4 个 android target，
  `~/.rustup/.../rustlib/aarch64-linux-android/lib/` 里 libcore/libstd 一个不少。
- 根因：cargo 是从 **PATH** 里找 `rustc` 的，而 rustup 垫片 exec cargo 时**不会替子进程重排 PATH**。
  本机 `/opt/homebrew/bin` 排在 `~/.cargo/bin` 前面（踩坑 1 的另一半），
  于是 cargo 实际用的是 **Homebrew 的 rustc**——它与 rustup 工具链同为 1.98.1（commit 相同），
  但**只带 host 的 std、没有任何交叉 target 的 std**。host 构建永远撞不到这一点
  （两条路径的 rustc 同版本、host std 都有），所以三条质量门从来没暴露过；
  只有第一次交叉编译才会炸，且报错文案（"target may not be installed"）会把人引向重装 target 的死胡同。
- 解法（两行，均已实测通过）：① `PATH="$HOME/.cargo/bin:$PATH"` 后再跑 cargo，
  或显式 `RUSTC` 指到工具链真身；② C 依赖（ring 经 cc-rs）需要 NDK clang，
  用环境变量接上即可、不必改仓库：
  `CC_aarch64_linux_android=…/aarch64-linux-android24-clang` 与 `AR_aarch64_linux_android=…/llvm-ar`。
  当前仓库已把 Android NDK 编译和打包收进 `script/android-build.sh` / CI；本地复跑证据与设备验收状态
  见 `docs/10-ANDROID-INTEGRATION.md`，不能把原始 E0463 当作产品代码编译失败。
- 教训：**"编译不过"要先分清是代码不行还是工具链拿错了**——E0463 指向 sysroot，
  先查 `cargo` 实际抓的哪个 rustc（`cargo -vV` 只看 cargo 不够，要看构建脚本里 rustc 的来历），
  再怀疑 target 没装。诊断时用 `rustc --target <t>` 直编一个空文件当探针，
  rustc 过而 cargo 不过 = PATH 顺序问题。**安卓落地的构建配置（PATH 约定或 `.cargo/config.toml`）
  要落盘进 docs/03 与打包脚本，不能靠"这台机器恰好能用"。**
  （已落盘：`script/android-build.sh` 启动即前置 `$HOME/.cargo/bin` 并导出 `CC_*` / `AR_*`；
  目标矩阵见 §6。）

### 45. Android 打包验收必须用普通应用 UID 和当前 targetSdk，adb shell 成功不是部署证明。

- 现象：最初测试 Manifest 的 targetSdk 是 28，不能覆盖报告中 targetSdk≥29 的可写目录执行限制；
  Instrumentation 直接调用不存在的 `getArguments` 也编译失败。
- 根因：构建成功、shell 执行成功与 APK 安装后执行是三个不同层次；
  测试框架的 onCreate 还必须显式 start 才触发 onStart。
- 解法：`script/android-smoke.sh`（实现在 `script/android-smoke.py`，测试 APK 的 Manifest
  模板为 `targetSdkVersion=35`）生成无 UI 临时 APK，从安装后的 `nativeLibraryDir` 执行。
  API 36、普通应用 UID 实测 HTTP 握手与 fixture 回放、本地 32,791 字节下载、stdio、C ABI 通过；
  公开 TLS 需 `XSPIDER_LIVE=1`，默认不联网；测试 APK 与新增的 `adb reverse` 映射用完即清理。
- 证据边界：模拟器页大小 4096，即使 ELF `LOAD=0x4000`、APK `zipalign -P 16` 全绿，
  也不能说真实 16 KB 设备已验收；账号 GraphQL、Doze 和 Android Aria2Next 同样单独验收。
  本轮逐项实测记录与「仍未验收」清单见 `docs/10-ANDROID-INTEGRATION.md` §8。
