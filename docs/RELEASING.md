# 发布规则（Release）

> **本文件是本仓库发布事务的唯一真源。** 不在根目录或其他位置维护第二份发布真源
> （无论其文件名叫什么）；发布语义（范围、资产、tag、验收、义务）的任何修改
> 只改这里。`docs/03-FFI-SIGNING-PACKAGING.md` 管的是「打包产物长什么样」，本文管的是
> 「怎么把它变成一次可追溯的发布」——两者冲突时，发布事务以本文为准。
>
> 适用仓库：`LeeDespo/x-spider-core`（[github.com/LeeDespo/x-spider-core](https://github.com/LeeDespo/x-spider-core)）
>
> 版本基线有两个维度，**永远分开写、分开报**（详见 §2）：
>
> | 维度 | 当前值 | 载体 |
> |---|---|---|
> | **对外契约版本** | **1.5.2** | `docs/CONTRACT.md`，由 `xspider_version()` 返回 |
> | **组件构建版本** | **0.1.0** | workspace `Cargo.toml`、`dist/` 包名、`xspiderd --version` 输出 |
>
> 当前发布范围：**macOS ARM64 双形态 + Android 两 ABI native 包**；Windows / Linux
> 不构建、不上传、不宣称（§3）。
>
> 语气约定：本文所有「必须 / 禁止 / 只」都是现行有效规则。凡「尚未落地」的事项，
> 在 §12 实施状态表中登记，**落地前任何人不得在 README、Release Notes 或其他文档中
> 宣称其已生效**。

---

## 1. 目标

GitHub Release 是本仓库的**正式交付面**。消费端（`x-spider-mac` 及未来外壳）**不依赖**：

- 本地 clone 本仓库源码；
- dirty working tree；
- 本地安装 Rust / Android NDK 后自行构建；
- 从聊天记录、网盘或 CI 日志里捞来历不明的二进制；
- 无法追溯到 commit、工具链与校验和的散装文件。

一次正式 Release 必须同时满足：

1. 所有发布物来自**同一个 Git tag**；
2. 构建源代码是 **clean tree**；
3. 每个资产可通过 **SHA-256** 验证；
4. Release 能明确追溯到 Git commit、**契约版本与组件版本两个号**、target 与构建工具链
   （manifest，§4.4–§4.5）；
5. 包内许可证件齐全（§7）；
6. Android 两个 ABI 的包同次构建、成套发布，不单独放行一个 ABI（§4.3）。

---

## 2. 版本基线与 tag 用哪个号

### 2.1 两个版本维度

| 维度 | 定义 | 当前值 | 在哪里可核对 |
|---|---|---|---|
| **对外契约版本** | `xspider_version()` 的返回值；method 与字段「只增不改不删」的兼容承诺单位 | **1.5.2** | `docs/CONTRACT.md` 头部；`contract/xspider.schema.json`；sidecar ready 行的 `version` 字段；`system.version` 的 `contract_version` |
| **组件构建版本** | workspace 版本号；语义化版本，跟随 `CHANGELOG.md` | **0.1.0** | 根 `Cargo.toml` 的 `version`；`dist/` 包名；`xspiderd --version` 输出；ready 行的 `build` 字段 |

实测核对（2026-10-06，本机 dist 包）：

```text
$ xspiderd --version
xspiderd 0.1.0 (契约版本 1.5.2)
```

ready 行形状（`docs/CONTRACT.md` §2.2）：`ready {"port":…,"token":"…","version":"1.5.2","build":"0.1.0"}`。
**两个号都要在 Release Notes 与 manifest 中出现**（§4.4、§9）；只报其中一个视为漏报。

### 2.2 tag 用组件版本，不用契约版本

```text
Git tag = v<组件构建版本>
```

理由（都是事实，不是偏好）：

- `dist/` 包名、`xspiderd --version`、ready 行 `build` 字段全是组件版本——tag 必须与
  消费端能亲手验到的号一致；
- 契约版本与组件版本**不同步推进**：首个 tag `v0.1.0` 指向的 commit（119979a）契约是
  1.5.1，当前 HEAD 契约已是 1.5.2 而组件版本未动。tag 名里放契约版本必然漂移；
- 契约版本由 `xspider_version()` / manifest / Release Notes 承载，不进 tag 名。

### 2.3 首个 tag 已存在

`git tag -l` 实测（2026-10-06）：仓库**已有且只有一个 tag**——

```text
v0.1.0 → 119979a75f83cdf5e8e8fe974c63a5f6e8abc027（契约 1.5.1 的提交）
```

它对应 GitHub 上已发布的 Release「X-Spider Core 0.1.0（契约 1.5.1）」（2026-10-04 发布，
资产为 `xspiderd-0.1.0-macos-arm64.tar.gz` + `.sha256`）。该次发布**先于本文件存在**，
按当时的手工流程完成，不追改；自下一版起，一切发布按本文执行（§5、§6）。

---

## 3. 当前发布范围矩阵

> 本矩阵**与 `docs/10-ANDROID-INTEGRATION.md` §8 的验证记录完全一致**，不许外推。
> 每一行都能在 `docs/10` §8 或 `docs/ROADMAP.md` 的 2026-10-05 台账里找到出处；
> 验收边界变化时，先改 `docs/10` §8，再同步本表。

| 平台 / 形态 | 验收状态 | 具体通过项 |
|---|---|---|
| **macOS ARM64**（`aarch64-apple-darwin`）：sidecar（HTTP / stdio）+ cdylib 双形态 | **已验收** | workspace 离线质量门（`cargo test` / clippy `-D warnings` / fmt）、双形态 + CLI 离线 smoke、release 打包与 ad-hoc 签名检查全绿（`docs/10` §8「macOS 回归」） |
| **Android**：NDK 构建 `arm64-v8a` + `x86_64` 两 ABI，sidecar + C ABI cdylib | **构建打包与模拟器冒烟通过**（边界见下） | NDK 27.3.13750724 / API 23 / rustc 1.98.1，两 ABI 实际链接打包通过；ELF 为 DYN、sidecar 带 PIE 标记、LOAD 对齐 ≥ 0x4000、三个 C ABI 导出齐全；API 36 模拟器（`emulator-5554`，targetSdk 35 测试 APK、普通应用 UID）从 `nativeLibraryDir` 执行：HTTP ready/token 握手、`system.version`、真实 fixture 回放取数、本地 32,791 字节下载逐字节一致后正常 shutdown；stdio fixture 调用通过；C 程序 `dlopen` 并调用/释放三个 C ABI 接口通过；`XSPIDER_LIVE=1` + 经 17890 的**无凭据**公开 TLS 探测（`https://x.com/robots.txt`）成功 |

Android **未验收**清单（发布物可以带，但任何文档、Release Notes、README **不得宣称**以下能力已验收，`docs/10` §8 原文照录）：

- X 账号 GraphQL live（真实账号取数）；
- x86_64 设备运行；
- API 23 实机；
- 真实 16 KB 页设备（仅有 ELF LOAD / APK `zipalign -P 16` 对齐检查；模拟器实际页大小 4096）；
- 前台服务 / Doze / phantom process 回收等后台生命周期；
- MediaStore 导出；
- Aria2Next Android 后端（`XSPIDER_ARIA2_PATH` 指向安卓资产）；
- 用户 CA / MITM 代理路径（`docs/10` §5 明确不属于支持路径）。

**Windows / Linux**：未开始。不构建、不上传、不在 README 与 Release Matrix 宣称支持
（ADR-038：CI 只保 macOS 格 + Android 构建检查；出现真实消费方时先加那一格的 CI，再加代码）。
CI 的 Linux runner 只用于 Android 构建，**不等于**发布 Linux 产物。

---

## 4. Release 资产清单与命名

### 4.1 资产总表（以组件版本 0.1.0 为例）

Release 页面只出现以下资产：

```text
xspiderd-0.1.0-macos-arm64.tar.gz
xspiderd-0.1.0-macos-arm64.tar.gz.sha256
xspiderd-0.1.0-android-arm64-v8a.tar.gz
xspiderd-0.1.0-android-arm64-v8a.tar.gz.sha256
xspiderd-0.1.0-android-x86_64.tar.gz
xspiderd-0.1.0-android-x86_64.tar.gz.sha256
release-manifest.json
SHA256SUMS
THIRD-PARTY-LICENSES.txt
```

以及 GitHub 自动提供的 `Source code (zip / tar.gz)`（与 tag 一一对应）。

命名规则（沿用 `script/package.sh` 与 `script/android-build.sh` 的现行规格，**不含 `v` 前缀**；
tag 的 `v` 前缀与资产名的无前缀是两个体系，不要互相"修正"）：

```text
xspiderd-<组件版本>-macos-<arch>.tar.gz        # macos-arm64
xspiderd-<组件版本>-android-<abi>.tar.gz       # android-arm64-v8a / android-x86_64
```

禁止模糊命名：`xspiderd.zip`、`release.tar.gz`、`latest.tar.gz`、裸的 `xspiderd`。

### 4.2 macOS 包

由 `script/package.sh` 产出（`docs/03` §5 结构）：

```text
xspiderd-0.1.0-macos-arm64/
├── xspiderd                 # sidecar 可执行文件（ad-hoc 签名；未签名会被内核 SIGKILL/137）
├── libxspider.dylib         # cdylib 次形态（产品名固定，ADR-012）
├── xspider.schema.json      # 机器可读契约
├── LICENSE                  # GPL-3.0-only
├── NOTICE                   # 第三方出处与义务
├── CHANGELOG.md
└── manifest.json            # §4.4（尚未落地）
```

可选项：设 `XSPIDER_ARIA2_PATH` 时随包携带 `aria2next`，此时必须同时放入
`LICENSE.aria2` 与 `ARIA2NEXT-NOTICE.txt`（版本 + 来源 + sha256，脚本已实现，`docs/03` §4 方案 A）。
**现行默认与 v0.1.0 实际发布均不携带 Aria2Next**（外壳自带或 `XSPIDER_ARIA2_PATH` 提供）。

### 4.3 Android 包

由 `script/android-build.sh` 产出，**每个 ABI 一个包**：

```text
xspiderd-0.1.0-android-arm64-v8a/
├── jniLibs/arm64-v8a/
│   ├── libxspiderd.so       # sidecar：PIE 可执行文件按 native-library 名打包
│   └── libxspider.so        # C ABI cdylib
├── xspider.schema.json
├── LICENSE  LICENSE.aria2  NOTICE
├── SHA256SUMS               # 包内两个 .so 的哈希（脚本已实现）
├── ANDROID-BUILD.txt        # ABI / target / NDK / API / 16 KB 对齐说明（脚本已实现）
├── test-fixtures/           # 仅离线验收用（Git 跟踪的脱敏 fixture）；生产 APK 不携带
└── manifest.json            # §4.4（尚未落地）
```

规则：

- **成套发布**：`arm64-v8a` 与 `x86_64` 任一 ABI 构建失败，整个 Android 发布失败，不放残缺包；
- `libxspiderd.so` 是可执行文件改名的部署技巧（`docs/10` §1–§2），包内 `ANDROID-BUILD.txt`
  必须随包说明，禁止消费端对它 `System.loadLibrary`；
- `test-fixtures/` 是验收便利，不是运行时依赖；外壳的 APK 打包不携带它。

### 4.4 每包 manifest.json（**尚未落地**）

`script/package.sh` 与 `script/android-build.sh` 目前都**不生成** manifest.json；
以下是两个脚本的实现规格，落地前包内以 `ANDROID-BUILD.txt` 与包内 `SHA256SUMS` 为准：

```json
{
  "name": "x-spider-core",
  "componentVersion": "0.1.0",
  "contractVersion": "1.5.2",
  "gitCommit": "<FULL_GIT_SHA>",
  "target": "aarch64-apple-darwin",
  "rustc": "<RUSTC_VERSION>",
  "files": {
    "xspiderd": "<SHA256>",
    "libxspider.dylib": "<SHA256>"
  }
}
```

Android 版另加 `"abi"`、`"androidNdk": "27.3.13750724"`、`"minApi": 23`、
`"pageAlignment": "16KiB-LOAD"`，`files` 覆盖两个 `.so`。

目的：只拿到解压目录的用户也能核对**两个版本号**、来源 commit、target 与完整性。

### 4.5 顶层 release-manifest.json / SHA256SUMS / THIRD-PARTY-LICENSES（**尚未落地**）

- `release-manifest.json`：描述整个 Release——`version`（组件）、`contractVersion`（契约）、
  `tag`、`gitCommit`、`sourceTreeClean: true`、toolchain（rustc / cargo / NDK）与每个资产的
  kind / target / sha256；
- `SHA256SUMS`：覆盖**全部**资产（含各 tar.gz、manifest 与许可证汇总），CI 生成、不手填。
  现状是每包一个独立的 `.sha256` 文件（已落地），顶层汇总文件未落地；
- `THIRD-PARTY-LICENSES.txt`：第三方依赖许可证汇总（Rust 依赖均为 MIT / Apache-2.0 宽松许可，
  `NOTICE` §4）。未落地前，包内 `NOTICE` + `LICENSE.aria2` 是现行有效的等价物，**不得因此省略**。

### 4.6 校验方法（写给消费端）

```bash
shasum -a 256 -c SHA256SUMS      # macOS
sha256sum -c SHA256SUMS          # Linux
```

---

## 5. tag 三位一体与 clean tree 纪律

### 5.1 三位一体

```text
Git tag v0.1.0  =  Cargo.toml version 0.1.0  =  dist 包名 xspiderd-0.1.0-*
```

不一致即停止发布。契约版本**不在**这个等式里——它单独记录在 Release Notes、
manifest 与 `docs/CONTRACT.md`（§2.2）。

### 5.2 禁止 dirty tree 发布

打 tag 前必须全部通过：

```bash
git diff --exit-code
git diff --cached --exit-code
test -z "$(git status --porcelain)"
```

任一失败即停止。注意第三条把**未跟踪且未被 ignore 的文件也算 dirty**（本仓库把
一次性启动简报、验收工作单、`dist/` 等本地工作文件列进了
`.gitignore`，不影响检查；`AGENTS.md` 已入库，临时杂物必须提交或忽略后再打 tag）。

### 5.3 Release 只能从 tag 经 CI 构建

```text
push tag v0.1.1 → GitHub Actions → checkout tag → 质量门 → 构建 → 打包 → 校验和 → Release
```

本地构建（`script/package.sh` / `script/android-build.sh`）只用于测试与验收演练；
**禁止本地编译后手工上传正式资产**。在 release.yml 落地前的过渡期，发布动作只能是
「按 §11 清单逐项验收后的手工上传」，且 tag / clean tree / 三位一体纪律照常生效——
这是过渡期的最小纪律，不是长期形态（§12）。

---

## 6. Release CI 实现规格（release.yml，**尚未创建**）

**实施状态：`.github/workflows/release.yml` 尚未创建**（`git ls-files .github/workflows`
实测只有 `ci.yml`）。本节是它的实现规格，实现前不得宣称有自动发布。

落地前置项：

1. **manifest 生成能力**：`package.sh` / `android-build.sh` 需先实现 §4.4 的 manifest.json
   与 §4.5 的顶层三件套生成（当前两个脚本均未生成）；
2. **真实 tag 环境**：触发依赖 tag push；`ci.yml` 现行触发条件是 branch push / PR /
   workflow_dispatch，不含 tag。

### 6.1 Trigger

只响应 `on: push: tags: ["v*"]`。不响应 branch push。

### 6.2 Job 结构

```text
validate
   │
   ├────────────┐
   ▼            ▼
macos       android（matrix: arm64-v8a / x86_64）
   │            │
   └──────┬─────┘
          ▼
       release
```

### 6.3 validate job

1. 校验 tag = `Cargo.toml` version（去掉 `v` 前缀后逐字相等，§5.1）；
2. clean tree 三连检查（§5.2）；
3. 三条质量门（离线）：`cargo test --workspace`、`cargo clippy --workspace --all-targets
   -- -D warnings`、`cargo fmt --all --check`；
4. `./script/smoke.sh`（离线：双形态 + CLI 端到端）。

### 6.4 macos job

- Runner `macos-latest`，只构建 `aarch64-apple-darwin`；
- `./script/package.sh`（release profile），脚本内含 ad-hoc 签名与 `codesign --verify --strict`
  自检、`xspiderd --version` 自检（package.sh 已实现）；
- 校验 `--version` 输出为 `xspiderd <tag 版本> (契约版本 <CONTRACT.md 头部版本>)`；
- 生成包内 manifest.json（前置项 1 落地后）。

### 6.5 android job

- Runner `ubuntu-latest`；复用 `ci.yml` 现有 `android-native` job 的做法：
  NDK 锁定 **27.3.13750724**（脚本有版本断言，`android-build.sh` 已实现）、
  Rust stable + 两个 android target、`RUSTFLAGS=-D warnings`；
- `./script/android-build.sh arm64-v8a x86_64`（或 matrix 每 ABI 一格，产物合并校验成套性）；
  脚本内含 ELF DYN / PIE / LOAD 16 KB 对齐 / 三导出符号检查（已实现）；
- **两 ABI 成套**：release job 收到两个 ABI 的包才允许继续（§4.3）；
- 生成包内 manifest.json（前置项 1 落地后）。

### 6.6 release job

仅当 validate / macos / android 全部成功后运行：下载全部 artifact → 组装资产 → 生成
`release-manifest.json` 与 `SHA256SUMS` → 生成/汇总 `THIRD-PARTY-LICENSES.txt` →
按 §9 模板生成 Notes → 创建 GitHub Release 并上传全部资产。

---

## 7. 许可证与分发义务

- 本仓库 **GPL-3.0-only**：请求构造、分页与解析逻辑移植自 GPL-3.0 的
  [MiningCattiva/x-spider](https://github.com/MiningCattiva/x-spider) 及其 macOS 移植
  `x-spider-mac`，衍生作品须沿用同一许可证；README 必须保留出处声明（现已具备，
  `README.md` §许可证）。
- **Aria2Next 是独立的 GPL-2.0 程序**（`AnInsomniacy/aria2-next`）：本组件通过
  「子进程 + JSON-RPC」调用，属聚合，不传染本仓库许可证；但**随包分发其二进制时必须**：
  附 GPL-2.0 完整文本（`LICENSE.aria2`）、提供源码获取方式、标明所用版本与 sha256
  （`NOTICE` §3；`script/package.sh` 的 `ARIA2NEXT-NOTICE.txt` 已实现）。Android 包
  无条件携带 `LICENSE.aria2`（`android-build.sh` 已实现）。本仓库**不 vendoring**
  Aria2Next 二进制。
- 每个发布包必须携带 `LICENSE` 与 `NOTICE`（macOS / Android 脚本均已实现）。
- 不提供任何面向公众的抓取服务，不发布抓取结果数据集；README 定位为
  个人自用、授权账号范围内的工具。

---

## 8. 消费端更新链

### 8.1 现行规则（已生效）

唯一已接入的消费方是 `x-spider-mac`（macOS SwiftUI 外壳）。它是本组件的**消费方**：
组件的查找顺序、部署目录与更新方式以外壳仓库自己的文档为准（其 `docs/DEVELOPMENT.md`
§2.7：**外部目录优先**；更新组件 = 换文件 + 重新 ad-hoc 签名，不必重新构建应用）。
与本仓库相关的两条纪律，外壳与发布侧都必须遵守：

```bash
DIR=~/Library/Application\ Support/moe.keli.xspider.mac/XSpiderCore
xattr -cr "$DIR"                                             # 清隔离属性（下载来的必做）
codesign --force --sign - "$DIR"/xspiderd "$DIR"/aria2next   # ad-hoc 签名
```

- 漏签 / 带隔离属性的典型表现是**退出码 137 静默死亡**（`docs/03` §1 实测）；
  外壳以「进程真的起来并完成握手」为绿灯判据，不以文件存在为判据。

### 8.2 本仓库的分发现状（如实）

- GitHub Release 渠道**已存在**：`v0.1.0`（2026-10-04 手工发布，契约 1.5.1，
  仅 macOS 资产；Android 包构建于其后，从未进入任何 Release）；
- 此前的分发现状 = `x-spider-mac` 随包携带组件 + 本地 `dist/` 打包
  （`dist/` 被 `.gitignore` 忽略，是本地构建产物，不是发布物）；
- **尚未建立**的是本文件定义的完整发布制度：release.yml 自动发布（§6）、
  manifest 三件套（§4.4–§4.5）、Android 资产进入 Release（§4.3）。

### 8.3 目标更新链（规格，尚未落地）

正式制度建立后，消费端更新方式固定为：

```text
锁定 Release 版本（外壳侧记录用哪个 Release）
        ↓
下载固定资产 + release-manifest.json + SHA256SUMS
        ↓
校验 SHA256 → 核对 manifest（两个版本号、commit）
        ↓
解压到组件目录（外部目录优先）→ xattr -cr + ad-hoc codesign
        ↓
启动握手核对契约版本（外壳已有：主版本 1.x 兼容检查）
```

在此之前，§8.1 的手工换文件 + 签名方式继续有效；§5 的 tag / clean tree / 三位一体纪律
对手工分发同样生效。

---

## 9. Release Notes 模板

每次 Release 使用统一结构（契约版本与组件版本**分两行**，§2.1）：

```markdown
# X-Spider Core <组件版本>（契约 <契约版本>）

## Compatibility

- Component: 0.1.0
- Contract (`xspider_version()`): 1.5.2

## Supported targets

- macOS ARM64 — sidecar（HTTP / stdio）+ cdylib；已验收
- Android — NDK 构建，arm64-v8a / x86_64 native 包；构建打包与 API 36 模拟器
  普通应用 UID 冒烟通过（HTTP/stdio/C ABI、本地下载、无凭据公开 TLS 探测）

## 未验收边界（Android，照录 docs/10 §8）

- X 账号 GraphQL live、x86_64 设备、API 23 实机、真实 16 KB 页设备（仅对齐检查）、
  前台服务/Doze、MediaStore、Aria2Next Android 后端、用户 CA

Windows / Linux 未发布、不宣称支持。

## Breaking / binding changes

- ...

## Added / Fixed

- ...

## Artifacts

- xspiderd-<版本>-macos-arm64.tar.gz
- xspiderd-<版本>-android-arm64-v8a.tar.gz
- xspiderd-<版本>-android-x86_64.tar.gz

用 `SHA256SUMS` 校验全部资产。

## License

- 本组件 GPL-3.0-only（源自 MiningCattiva/x-spider 移植，见包内 LICENSE 与 NOTICE）
- Aria2Next（如随包携带）为独立 GPL-2.0 程序，附 LICENSE.aria2 与源码获取方式
```

---

## 10. 禁止事项

```text
从 dirty working tree 打 tag / 发布
手工上传本地构建的正式资产（release.yml 落地后；过渡期仅限按 §11 验收后的手工发布）
tag 与 Cargo.toml 组件版本不一致
把契约版本与组件版本混成一个号，或 tag 名里放契约版本
只发布两个 Android ABI 中的一个
发布包缺 LICENSE / NOTICE；携带 aria2next 却缺 LICENSE.aria2 与来源说明
发布未验收的 Windows / Linux 产物，或在任何文档宣称其支持
宣称 Android 未验收清单（§3）中的能力已验收
把未脱敏的 fixtures/raw/ 或任何凭据带进发布物
宣称「绕过 X 限流」或以抓取服务/数据集形式再分发
把 .rlib / 裸 staticlib 当成通用发布物
把发布真源挪出本文件（在别处另建发布规则文档）
```

---

## 11. 发布前验收清单

逐项核对；这也是 release.yml 各 job 的断言来源（§6）。

**版本与源码**

- [ ] tag = `v` + `Cargo.toml` version（§5.1）
- [ ] `CHANGELOG.md` 的「未发布」节已落成对应版本小节
- [ ] clean tree 三连检查通过（§5.2）
- [ ] `docs/CONTRACT.md` 头部契约版本与代码一致（契约守卫测试已覆盖）

**质量门（全部离线）**

- [ ] `cargo test --workspace` / clippy `-D warnings` / fmt 全绿
- [ ] `./script/smoke.sh`（双形态 + CLI）通过

**macOS 包**

- [ ] `script/package.sh` 产物含 xspiderd / libxspider.dylib / schema / LICENSE / NOTICE / CHANGELOG
- [ ] ad-hoc 签名 + `codesign --verify --strict` 通过（脚本自检）
- [ ] `xspiderd --version` 输出两个版本号正确

**Android 包（两 ABI 成套）**

- [ ] `script/android-build.sh arm64-v8a x86_64` 两包均产出
- [ ] ELF DYN / PIE / LOAD 16 KB 对齐 / 三导出符号检查通过（脚本自检）
- [ ] 包内 SHA256SUMS、ANDROID-BUILD.txt、LICENSE.aria2、NOTICE 齐全

**Release 资产**

- [ ] 六个 tar.gz/.sha256 资产 + `release-manifest.json` + `SHA256SUMS` +
      `THIRD-PARTY-LICENSES.txt` 全部上传（后三者落地后）
- [ ] Release Notes 按 §9 模板，含两个版本号与 Android 未验收边界
- [ ] Source archives 与 tag 对应

**不支持的平台**

- [ ] 未上传 Windows / Linux 产物；README 与 Notes 未宣称支持

---

## 12. 实施状态

规则自本文件起生效。以下事项**尚未落地**，完成时回填本节：

| 事项 | 状态 | 依赖 |
|---|---|---|
| GitHub Release 渠道 | **已存在**：v0.1.0（2026-10-04 手工发布，macOS 资产，契约 1.5.1） | — |
| `.github/workflows/release.yml` | 未创建 | §6 前置项 1、2 |
| 每包 manifest.json 生成 | 未落地（`package.sh` / `android-build.sh` 均未生成） | §4.4 规格 |
| 顶层 release-manifest.json / SHA256SUMS / THIRD-PARTY-LICENSES.txt | 未落地（现为每包 `.sha256` + 包内 NOTICE/LICENSE.aria2） | 随 release.yml 或先行脚本化 |
| Android 资产进入 Release | 未发生（v0.1.0 无 Android 资产） | 上两行落地后随下一版发布 |
| `x-spider-mac` 改为消费 Release 固定资产 | 未发生（现状：随包携带 + 外部目录手工替换 + 签名，§8.1） | 本仓库首个按新制度发布的 Release |

过渡期（上表未落地期间）的分发维持 §8.2 现状，但 §5 的 tag / clean tree / 三位一体纪律、
§7 的许可证义务与 §10 的禁止事项，从现在起对**任何手工分发**同样生效。

---

## 结论

当前阶段的发布范围就是两条正式交付链：

```text
1. macOS ARM64 → xspiderd（sidecar，HTTP/stdio）+ libxspider.dylib（cdylib）
2. Android    → arm64-v8a / x86_64 两 ABI native 包（sidecar + C ABI cdylib）
```

Windows / Linux 未开始，不进入 Release。比「发布尽可能多的平台」更重要的是这条等式始终成立：

```text
一个 tag = 一个 clean commit = 一个组件版本
        = 一组成套、可验证 SHA256 的发布资产
        = Notes 里同时写清的「组件版本 + 契约版本」
```
