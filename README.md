# X-Spider Core

X（Twitter）数据获取与下载的 **Rust 核心组件**，供不同平台的「外壳应用」复用：
外壳只管界面与产品逻辑，取数、限流、爬取、下载、校验由本仓库提供。

- **主形态**：sidecar 可执行文件 + 本地 JSON-RPC —— 换组件 = 换一个二进制，跨平台一份二进制；
- **次形态**：cdylib（`libxspider.dylib` / `.so` / `.dll`）—— 给需要进程内调用的外壳。

预期消费方：`x-spider-mac`（macOS SwiftUI），以及将来可能出现的 Windows / Linux 外壳。

> **状态：核心 M0–M4 已完成**（2026-10-01）；当前契约为 1.5.2 PATCH。
> 下载队列支持持久化状态恢复：Waiting/Active 重启后变为 Waiting，Paused 保持暂停，
> Error（包括取消）不会自动重试；取消任务不会自动复活，用户可在清理结束后显式恢复。
> Android NDK 构建、sidecar 打包与构建检查已实现；模拟器/设备、Aria2Next Android 二进制与 live 网络验收状态见 [`docs/10-ANDROID-INTEGRATION.md`](docs/10-ANDROID-INTEGRATION.md)。
> 7 个取数端点 + 两个下载后端（内置 HTTP / **Aria2Next**）
> + 下载队列（幂等 / 暂停恢复 / 重启状态恢复）+ 爬取调度（候选清单 + `done_reason`），
> 全部有真实响应 fixture 或真二进制 E2E 覆盖；另有一个**只经契约**的 CLI 当第一个真实消费方
> （`bins/xspider-cli`）。进度与未决见 [`docs/ROADMAP.md`](docs/ROADMAP.md) 与
> [`docs/06-CONSUMER-INTEGRATION.md`](docs/06-CONSUMER-INTEGRATION.md)。

## 组件

| 组件 | 职责 | 状态 |
|---|---|---|
| `crates/xspider-core` | 共享内核：HTTP 客户端、凭据注入、限流闸门与 429 熔断、请求签名、错误分类 —— **不对外发布** | ✅ |
| `crates/xspider-fetch` | 取数：用户、媒体/推文时间线、推文详情、搜索、主页时间线、关注、关注态、写操作 | ✅ 7 个读端点 + `fetch.is_following` + `fetch.mutate` |
| `crates/xspider-download` | 爬取调度 + 下载：翻页、筛选、队列、并发、断点续传、完整性校验 | ✅ 两个后端 + 队列 + 爬取调度 |
| `crates/xspider-ffi` | C ABI（三个函数）+ method 派发 | ✅ |
| `bins/xspiderd` | sidecar：本地 JSON-RPC | ✅ |
| `bins/xspider-cli` | **第一个真实消费方**：只经契约驱动组件（取一页 → 下 N 个媒体 → 报告） | ✅ |

依赖方向是单向的：`ffi / sidecar → 两个组件 → core`。**两个组件之间不互相依赖**
（爬取与下载通过外壳的「候选清单」衔接）。

## 对外契约

只有三个 C ABI 函数，所有能力都走一个入口：

```c
char* xspider_version(void);                                  // 契约版本握手，如 "1.5.1"
char* xspider_call(const char* method, const char* json_in);  // 所有能力
void  xspider_free(char* ptr);                                // 释放返回的字符串
```

**新增能力 = 新增一个 method 字符串**（加法式演进，外壳无需改动）。
契约里不会出现端点路径、queryId、`features` 常量或 HTTP 头——那些是组件的实现细节，
写进契约就等于把契约焊死在今天的 X 上（有自动化测试守着这一条）。

- 人读版：[`docs/CONTRACT.md`](docs/CONTRACT.md)
- 机器可读版：[`contract/xspider.schema.json`](contract/xspider.schema.json)
- 变更记录：[`CHANGELOG.md`](CHANGELOG.md)

当前共 **26 个 method**：`system.*`（2）、`auth.*`（2）、`net.*`（4，含 `net.probe_size`）、
`fetch.*`（9：7 个读端点 + `fetch.is_following` + 写操作 `fetch.mutate`）、`dl.*`（8）、`crawl.run`（1）。

逐 method 的**作用与读写属性**见 [`docs/09-METHOD-INDEX.md`](docs/09-METHOD-INDEX.md)，
每行链到 [`docs/07-API-REFERENCE.md`](docs/07-API-REFERENCE.md) 的详细小节。

## 前提

- **Rust 工具链**：用 [rustup](https://rustup.rs) 安装。本仓库用 `rust-toolchain.toml` 锁定工具链，
  但 **Homebrew 装的 `cargo` 是真实二进制、不读该文件**；若它排在 PATH 前面，锁定会被静默忽略——
  请确保 rustup 的 `~/.cargo/bin` 排在前面（或直接用 `~/.cargo/bin/cargo`）：
  `export PATH="$HOME/.cargo/bin:$PATH"`。
- **macOS**：需要 Command Line Tools 提供的 `cc`（冒烟脚本会编译 `script/cdylib_check.c`）：
  运行 `xcode-select --install`。
- **python3**：`script/smoke.sh` 与 fixture 脱敏脚本用到（系统自带即可）。
- 依赖已缓存在本机时可用 `--offline`（更快也更稳）；首次或改过依赖需先联网 `cargo fetch`。

## 快速开始

```bash
# 默认离线：不碰网络，用真实响应 fixture 回放
./script/smoke.sh
```

冒烟脚本会一次跑完：构建 → `dlopen` 验 cdylib 的三个符号 → 起 sidecar 读 ready 行
→ `curl` 调 `system.version` 与 `fetch.get_user` 并断言字段 → 关停 → 断言无残留进程。

下载与爬取（全部离线，用本地 HTTP fixture server）：

```bash
cargo test -p xspider-download                    # 单元 + 队列 E2E + HTTP E2E + Aria2Next E2E
cargo test -p xspider-download --test aria2_e2e   # 8 条真二进制 E2E（含"404 被引擎报成成功"）
```

CLI（第一个真实消费方，只经契约；离线也能跑）：

```bash
# 离线：fixture 回放，只规划不下载（CI 友好）
cargo run -p xspider-cli -- --screen-name demo_user --fixture-dir fixtures --dry-run

# 看组件支持哪些 method（外壳的启动自检）
cargo run -p xspider-cli -- --list-methods --fixture-dir fixtures

# live：真的取一页、下 3 个媒体（需要凭据与代理）
cargo run -p xspider-cli -- --screen-name tesla --count 3 --out ./downloads \
  --proxy "$XSPIDER_PROXY" --segments 4
```

它不链接任何本仓库的 crate——契约缺什么、哪里别扭，它第一个炸。
接入手册与实测记录见 [`docs/06-CONSUMER-INTEGRATION.md`](docs/06-CONSUMER-INTEGRATION.md)。

打包：

```bash
./script/package.sh                                        # 产出 dist/xspiderd-<ver>-<平台>-<架构>.tar.gz
XSPIDER_ARIA2_PATH=/path/to/aria2next ./script/package.sh   # 顺带带上 Aria2Next（附 GPL-2.0 声明）
```

Android native libraries / sidecar 构建包（需要 rustup 与 NDK 27.3.13750724；输出不是 APK）：

```bash
export ANDROID_NDK_HOME="$HOME/Library/Android/sdk/ndk/27.3.13750724"
./script/android-build.sh
```

手动起 sidecar：

```bash
cargo run -p xspiderd -- --port 0     # stdout 打印一行：ready {"port":N,"token":"...","version":"1.5.1"}

# 另开一个终端，用上面读到的 port 与 token
curl -s http://127.0.0.1:$PORT/ -H "X-XSpider-Token: $TOKEN" -H 'Content-Type: application/json' \
     -d '{"method":"fetch.get_user","params":{"screen_name":"jack"}}'
```

拿真实数据（需要你自己的账号 cookie 与可用的代理）：

```bash
cargo run -p xspiderd -- --port 0
# 然后：auth.set_cookie → fetch.get_user
```

**凭据只进不出**：cookie 不落盘、不打日志、不回传，由外壳通过 `auth.set_cookie` 注入。

## 文档

| 文件 | 内容 |
|---|---|
| [`docs/CONTRACT.md`](docs/CONTRACT.md) | 对外契约：method 表、字段、错误码、版本策略 |
| [`docs/09-METHOD-INDEX.md`](docs/09-METHOD-INDEX.md) | **接口清单**：26 个 method 的作用、只读/写属性，逐行链到接口参考 |
| [`docs/07-API-REFERENCE.md`](docs/07-API-REFERENCE.md) | **接口参考**：26 个 method 的入参/出参、数据形状、错误处理、调用时序 |
| [`docs/08-CAPABILITY-MAP.md`](docs/08-CAPABILITY-MAP.md) | **能力对照**：组件 ↔ `x-spider-mac` 的逐项映射、分工与缺口 |
| [`docs/10-ANDROID-INTEGRATION.md`](docs/10-ANDROID-INTEGRATION.md) | Android NDK 构建、sidecar 部署与外壳接入边界；列出尚未完成的设备验收 |
| [`docs/11-REVIEW-RESOLUTION.md`](docs/11-REVIEW-RESOLUTION.md) | 本轮审阅发现的归并处置与未验收边界 |
| [`docs/06-CONSUMER-INTEGRATION.md`](docs/06-CONSUMER-INTEGRATION.md) | 接入手册（给具体某个外壳）：改动清单、实测记录 |
| [`docs/DECISIONS.md`](docs/DECISIONS.md) | 决策台账（ADR）：每条决定及其「何时该被推翻」 |
| [`docs/ROADMAP.md`](docs/ROADMAP.md) | 里程碑、实际进度、风险台账 |
| [`CHANGELOG.md`](CHANGELOG.md) | 对外可见的变化 |
| [`NOTICE`](NOTICE) | 第三方组件出处与 GPL 义务（含 Aria2Next） |
| [`docs/01-ARCHITECTURE.md`](docs/01-ARCHITECTURE.md) | 组件边界、契约形状、下载引擎三种后端、任务归属 |
| [`docs/02-X-DOMAIN-NOTES.md`](docs/02-X-DOMAIN-NOTES.md) | X 领域知识与实测踩坑清单 |
| [`docs/03-FFI-SIGNING-PACKAGING.md`](docs/03-FFI-SIGNING-PACKAGING.md) | FFI / 签名 / 分发 / 交叉编译的实测结论 |
| [`docs/04-TESTING-AND-FIXTURES.md`](docs/04-TESTING-AND-FIXTURES.md) | 测试策略：fixture 回放、live canary、契约测试 |
| [`docs/05-WORKFLOW.md`](docs/05-WORKFLOW.md) | 工作循环、ADR 纪律、工程陷阱清单 |
| [`fixtures/README.md`](fixtures/README.md) | fixture 怎么产生、覆盖度、**哪些缺口是有意留的** |

## 质量门

```bash
cargo test --workspace            # 默认离线，不碰网络
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
```

真网络测试一律需要 `XSPIDER_LIVE=1` 门控，且默认 `#[ignore]`
（会消耗账号配额，不要在 CI 里默认跑）。

## 许可证

**GPL-3.0-only**。请求构造、分页与解析逻辑移植自 GPL-3.0 的
[`MiningCattiva/x-spider`](https://github.com/MiningCattiva/x-spider) 及其 macOS 移植，
衍生作品须沿用同一许可证。第三方组件的出处与义务（含 aria2-next 的 GPL-2.0 声明）
见 [`NOTICE`](NOTICE)。

## 定位

个人自用、**授权账号范围内**的工具。不提供任何面向公众的抓取服务，不发布抓取数据集，
也不以「绕过平台限流」为目的——限流是这里的一等公民：所有请求都过统一的限流闸门，
接口与媒体 CDN 的配额分开治理，触发限流后熔断冷却而不是继续重试。
