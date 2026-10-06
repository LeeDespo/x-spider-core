<div align="center">
  <h1>X-Spider Core</h1>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/Rust-edition%202021-orange" alt="Rust"></a>
  <a href="https://github.com/LeeDespo/x-spider-core/actions/workflows/ci.yml"><img src="https://github.com/LeeDespo/x-spider-core/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <img src="https://img.shields.io/badge/calling%20face-JSON--RPC%20%2F%20C%20ABI-blue" alt="Calling face">
  <a href="https://github.com/LeeDespo/x-spider-core/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-GPL--3.0--only-green" alt="License"></a>
</div>

---

> [!IMPORTANT]
> 本项目定位于**授权账号范围内**的个人数据获取、下载与跨平台组件研究。
> 不提供面向公众的抓取服务，不发布抓取数据集，也不以绕过平台限流为目标——
> 限流是这里的一等公民：所有请求过统一闸门，接口与媒体 CDN 的配额分开治理，
> 触发限流后熔断冷却而不是继续重试。凭据由使用者自己注入，平台规则与适用法律
> 由使用者自行遵守。

## 📖 介绍

X（Twitter）数据获取与下载的 **Rust 核心组件**：外壳应用只管界面与产品逻辑，
取数、限流、爬取、下载、校验由本组件提供。两套交付形态，共用同一份实现、行为一致：

- **sidecar 可执行文件 + 本地 JSON-RPC（主形态）**——换组件 = 换一个二进制；
- **cdylib（次形态）**——给需要进程内调用的外壳（`libxspider.dylib` / `.so` / `.dll`）。

对外只有三个 C ABI 函数，所有能力走一个 method 字符串入口（加法式演进，外壳无需改动）：

```c
char* xspider_version(void);                                  // 契约版本握手
char* xspider_call(const char* method, const char* json_in);  // 所有能力
void  xspider_free(char* ptr);                                // 释放返回字符串
```

契约里不会出现端点路径、queryId、`features` 常量或 HTTP 头——那些是组件的实现细节，
写进契约就等于把契约焊死在今天的 X 上（有自动化测试守着这一条）。

> 当前契约版本：1.5.2（以 docs/CONTRACT.md 为准）

`x-spider-mac`（macOS SwiftUI）是已接入的真实消费方；`bins/xspider-cli` 是仓库自带的
**第一个真实消费方**——它不链接任何本仓库 crate（守卫测试强制），只经契约驱动组件，
契约缺什么、哪里别扭，它第一个炸。

## 🧭 多端适配程度

| 调用面 / 平台 | 现状 |
|---|---|
| **macOS ARM64**：sidecar（HTTP / stdio）+ cdylib | ✅ **正式**：离线质量门、双形态 + CLI smoke、release 打包与 ad-hoc 签名检查通过 |
| **Android**（NDK，arm64-v8a / x86_64） | ⚠️ **部分验证**：构建打包与 API 36 模拟器普通应用 UID 冒烟通过；账号 GraphQL live、真实 16 KB 页设备、Doze、Aria2Next Android 后端未验收 |
| **Windows / Linux** | ❌ 未适配 |

「能构建」不等于「正式支持」：逐项验收边界与证据以
[`docs/10-ANDROID-INTEGRATION.md`](docs/10-ANDROID-INTEGRATION.md) §8、
[`docs/RELEASING.md`](docs/RELEASING.md) §3 与
[`docs/ROADMAP.md`](docs/ROADMAP.md) 为准。

## 🚀 快速开始

前提：[rustup](https://rustup.rs) 安装的 Rust 工具链（本机 PATH 里 Homebrew 的 cargo
不读 `rust-toolchain.toml`，请确保 `~/.cargo/bin` 排在前面）；macOS 需要 Command Line
Tools 的 `cc`（`xcode-select --install`）；`python3`（系统自带即可）。

```bash
# 一键离线冒烟：构建 → cdylib dlopen → sidecar 握手 → 调用断言 → 无残留
./script/smoke.sh

# CLI（第一个真实消费方）离线示例：fixture 回放、只规划不下载
cargo run -p xspider-cli -- --screen-name demo_user --fixture-dir fixtures --dry-run

# 手动起 sidecar（stdout 打印一行 ready：port / token / version / build）
cargo run -p xspiderd -- --port 0
curl -s http://127.0.0.1:$PORT/ -H "X-XSpider-Token: $TOKEN" -H 'Content-Type: application/json' \
     -d '{"method":"fetch.get_user","params":{"screen_name":"jack"}}'

# Android native 包（需要 NDK 27.3.13750724；输出不是 APK）
export ANDROID_NDK_HOME="$HOME/Library/Android/sdk/ndk/27.3.13750724"
./script/android-build.sh
```

**凭据只进不出**：cookie 不落盘、不打日志、不回传，由外壳通过 `auth.set_cookie` 注入。
完整命令手册（下载 E2E / live / 录制 / 脱敏 / sidecar 调试）见
[`docs/05-WORKFLOW.md`](docs/05-WORKFLOW.md) §9；正式打包与 Release 规则见
[`docs/RELEASING.md`](docs/RELEASING.md)。

## ✨ 能力概要

**取数**：用户、媒体 / 推文时间线、推文详情树、搜索、主页时间线、关注关系与互动写操作
（点赞 / 转推 / 书签 / 关注，成败看返回体而不只看状态码）。

**爬取**：分页调度、时间区间、候选筛选、结束原因（`done_reason`）、完整推文回传。

**下载**：内置 HTTP 与 Aria2Next 双后端、下载队列（幂等 / 暂停恢复 / 取消）、断点续传、
完整性校验、重启状态恢复。

**运行时**：凭据注入、代理热切换、统一限流与 429 熔断、sidecar 生命周期与父进程看门狗。

完整接口以 [`docs/09-METHOD-INDEX.md`](docs/09-METHOD-INDEX.md) 为准（逐行链到
[`docs/07-API-REFERENCE.md`](docs/07-API-REFERENCE.md) 的详细小节）。

## 🏗️ 组件结构

| 组件 | 职责 |
|---|---|
| `crates/xspider-core` | 共享内核：HTTP 客户端、凭据注入、限流熔断、签名、错误分类（不对外发布） |
| `crates/xspider-fetch` | 取数：用户 / 时间线 / 详情 / 搜索 / 关注 / 写操作 |
| `crates/xspider-download` | 爬取调度 + 下载：翻页、筛选、队列、并发、断点续传、完整性校验 |
| `crates/xspider-ffi` | C ABI（三个函数）+ method 派发 |
| `bins/xspiderd` | sidecar：本地 JSON-RPC |
| `bins/xspider-cli` | 仓库自带的契约消费方（守卫测试强制它不链接任何 crate） |

```text
sidecar / C ABI      ← 外壳从这里接入（HTTP ↔ cdylib 切换只换传输，契约载荷不变）
      ↓
fetch / download     ← 两个组件互不依赖（经外壳的「候选清单」衔接）
      ↓
core（共享内核）
```

依赖方向是单向的；分层与边界详见 [`docs/01-ARCHITECTURE.md`](docs/01-ARCHITECTURE.md)。

## 📚 文档导航

* **[docs/README.md](docs/README.md)** —— 完整文档索引（所有主题文档从这里进）
* **[docs/CONTRACT.md](docs/CONTRACT.md)** —— 对外契约（method 表、字段、错误码、版本策略）
* **[docs/09-METHOD-INDEX.md](docs/09-METHOD-INDEX.md)** —— 接口入口
* **[docs/06-CONSUMER-INTEGRATION.md](docs/06-CONSUMER-INTEGRATION.md)** —— 消费端接入手册
* **[docs/RELEASING.md](docs/RELEASING.md)** —— 发布规则（唯一真源）
* **[docs/ROADMAP.md](docs/ROADMAP.md)** —— 当前路线图
* **[AGENTS.md](AGENTS.md)** —— 开发 / Agent 规范入口（开工先读）

## 📄 许可证

**GPL-3.0-only**（[LICENSE](LICENSE)）。请求构造、分页与解析逻辑移植自 GPL-3.0 的
[`MiningCattiva/x-spider`](https://github.com/MiningCattiva/x-spider) 及其 macOS 移植，
衍生作品须沿用同一许可证。第三方组件的出处与义务（含 aria2-next 的 GPL-2.0 声明）见
[NOTICE](NOTICE)。

## ⚠️ 使用说明

个人自用工具。使用本项目产生的一切后果由使用者自行负责；请遵守 X 的服务条款与
所在地区适用法律，仅在自己的授权范围内使用。
