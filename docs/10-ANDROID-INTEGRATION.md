# 10 · 安卓接入与验证边界

> 契约 1.5.2；26 个 method、三个 C ABI 导出保持不变。安卓先采用 **sidecar + HTTP + 内置下载后端**（ADR-042），复用 macOS 的 JSON 载荷。这里提供组件构建与外壳接线方式，不把交叉编译通过当作 APK/后台/live 验收。

## 1. 构建和产物

使用 Rust **rustup** 工具链及 Android NDK 27.3.13750724，API 下限 23。将 `~/.cargo/bin` 放在 PATH 首位：仅指定 cargo 路径但仍用 Homebrew rustc，会误报缺少 Android std（报告的编译错误已更正）。

```sh
export ANDROID_NDK_HOME="$HOME/Library/Android/sdk/ndk/27.3.13750724"
./script/android-build.sh
```

脚本参数、输出目录与设备冒烟入口以 `script/android-build.sh --help` 和 `script/android-smoke.sh --help` 为准。arm64-v8a 对应 `aarch64-linux-android`；x86_64 对应 `x86_64-linux-android`，用于相应模拟器。二者不可混用。Android 15 的 16 KB 设备要求原生 ELF 兼容，脚本显式设置 16 KB LOAD 段对齐；APK 的包装对齐仍需由外壳构建检查。[官方页大小说明](https://developer.android.com/guide/practices/page-sizes)

交付中的 `libxspiderd.so` 是 **PIE 可执行文件改名**，用 ProcessBuilder 启动；`libxspider.so` 是 C ABI 动态库，只在 JNI 形态中加载。不要对 `libxspiderd.so` 调 `System.loadLibrary`。生产包不要附带 fixture；它只用于离线验收。

## 2. APK 中部署 sidecar

面向 API 29+ 的应用不能在应用 home 目录执行文件；将二进制从 assets 复制到 filesDir 再 chmod 的桌面式部署会失败。[Android 10 执行权限变化](https://developer.android.com/about/versions/10/behavior-changes-10#execute-permission)

把对应 ABI 的 `libxspiderd.so` 放在 `app/src/main/jniLibs/<abi>/`，让安装器提取到 `applicationInfo.nativeLibraryDir`。AGP 配置示例（Kotlin DSL）：

```kotlin
android {
    packaging {
        jniLibs {
            useLegacyPackaging = true
            // 此 .so 实际是可执行文件，保留组件构建时已验证的产物。
            keepDebugSymbols += "**/libxspiderd.so"
        }
    }
}
```

若发布 AAB，应检查生成的 **实际安装 APK** 同样提取 native 文件，而非仅验证开发 APK；需要时同步设置 AGP 的 `useLegacyPackagingFromBundle`。使用 nativeLibraryDir 的实体路径启动，不能执行 APK ZIP 中的条目。[AGP 包装 API](https://developer.android.com/reference/tools/gradle-api/8.3/com/android/build/api/variant/JniLibsApkPackaging)

先用内置后端即可下载，无需 aria2。若随 APK 分发 Aria2Next，另将经安卓验证的可执行资产命名为 `libaria2next.so`，放同一 ABI 目录，经 `XSPIDER_ARIA2_PATH` **显式指定**；不删除 macOS 原来的查找候选。许可证与 NOTICE 放 APK assets 或外壳的开源许可页面，包含 LICENSE.aria2 与 NOTICE 所列源码获取方式。仅看到上游 android-arm64 资产，不能算下载后端验收。

## 3. 启动与目录配置

下面是启动片段；整个启动、管道读取、HTTP 调用、等待退出都应在后台线程执行。外壳需要为读 ready 行设置超时，同时持续消费 stderr，避免管道堵塞。

```kotlin
val executable = File(context.applicationInfo.nativeLibraryDir, "libxspiderd.so")
val stateDir = File(context.filesDir, "xspider-state").apply { mkdirs() }
val cacheDir = File(context.cacheDir, "xspider").apply { mkdirs() }
val builder = ProcessBuilder(
    executable.absolutePath, "--port", "0", "--state-dir", stateDir.absolutePath
)
builder.environment()["TMPDIR"] = cacheDir.absolutePath
builder.environment()["XSPIDER_ARIA2_DIR"] = cacheDir.absolutePath
// 只有确实部署、验收了安卓 Aria2Next 才设置 XSPIDER_ARIA2_PATH。
val process = builder.start()
```

- HTTP 模式从 **stdout** 的 `ready {...}` 读取 port/token/version，持续消费 stderr 日志；不要用 `redirectErrorStream(true)` 混入握手通道。
- `--state-dir` 优先于 `XSPIDER_STATE_DIR`；Unix/Android 两种配置方式都取得内核独占锁。锁文件存在不表示实例仍在运行：进程死后内核自动解锁，文件保留以避免删除 inode 的竞态。
- 为每个独立组件实例提供不同状态目录；同一目录的第二个实例会在 ready 前报错退出。
- 版本主版本须兼容，且 `system.methods` 包含外壳所需方法。凭据用 `auth.set_cookie` 注入，不放启动参数或日志。
- 正常退出先调 `system.shutdown` 并等待，超时再终止子进程。意外死亡则重起、重新注入凭据/代理/限流配置、用 `dl.list()` 对账。
- 未给状态目录仍是原来的内存模式；外壳应将状态目录作为启动必填配置，不能期望内存任务在重启后出现。

## 4. HTTP 与 stdio 的取舍

使用 HttpURLConnection、OkHttp 等安卓 HTTP 栈访问本地 HTTP 时，targetSdk >=28 的默认明文规则需要考虑。仅放行组件实际使用的 `127.0.0.1`，而非全局开放明文；API 37+ 在没有显式 localhost 配置时有隐式 localhost 规则，因此报告中的“永远没有豁免”并非当前所有版本的事实。[官方网络安全配置](https://developer.android.com/privacy-and-security/security-config)

`res/xml/network_security_config.xml`：

```xml
<network-security-config>
    <base-config cleartextTrafficPermitted="false" />
    <domain-config cleartextTrafficPermitted="true">
        <domain>127.0.0.1</domain>
    </domain-config>
</network-security-config>
```

Manifest 的 application 引用 `android:networkSecurityConfig="@xml/network_security_config"`，并声明 `android.permission.INTERNET`。该配置管理安卓 HTTP 栈，不替代 Rust rustls 的 TLS 校验。

备选 `--stdio` 不需要本地端口及明文配置：ready 在 **stderr**（无 port/token），stdout 每行一个响应。外壳写入每个 JSON 请求后换行并 flush，用 id 配对；stdin EOF 使组件退出。**请求串行**，长 `crawl.run` 阻塞后续 `dl.pause` / `dl.events`；用小 `max_pages` 分块爬取。需要独立取消在飞调用时优先 HTTP（断开请求的取消路径），不要把 stdio 当作并发 RPC。

## 5. 网络、TLS 与登录

安卓 target 在原来的 native-roots 探测之外附加内置公共 webpki 根证书，解决 native-roots 在普通应用只探测到 Termux 路径而信任库为空的问题；其他平台继续使用系统 native roots。fetch、CDN、大小探测与自愈辅助请求共用该构建配置；代理热切换不改变信任源。

**不关闭证书校验**。Rust 客户端不自动继承 WebView/Java 的网络安全配置或用户导入 CA。自签 CA/MITM 重签代理不属于当前支持路径；使用保持原站 TLS 的隧道代理，或由外壳 VPN 透明转发。

| 网络环境 | 接线 |
|---|---|
| 可直连 | 缺省不指定代理；若要忽略环境变量，`net.set_proxy {url:null}` |
| 用户在外壳内设置代理 | 初始化和变化时调用 `net.set_proxy`，取数与下载同时生效 |
| VPN 透明代理 | 由系统路由转发，通常无需重复设置代理 |

组件不自动读取安卓 Wi-Fi 代理设置；外壳需要自己转换成契约输入。遇到 `transport` 先检查出口，429 冷却快速返回 `rate_limited`，不会阻塞 15 分钟等待。

登录由外壳负责，可用 WebView 的 `CookieManager.getInstance().getCookie("https://x.com")` 取得已授权完整会话串，再调 `auth.set_cookie`。持久化由外壳使用 Android Keystore 支持的加密方案决定；组件不保存凭据。禁止输出 cookie/token 到 logcat、崩溃报告、HTTP 调试拦截器或 UI 日志。

## 6. 后台、恢复与存储

活跃下载需要外壳按当前 Android 版本管理前台服务、通知及 `dataSync` 对应权限。前台服务不能保证 sidecar 永不被系统回收。target Android 15+ 时后台 dataSync 服务存在时间预算；在超时回调中暂停任务并结束服务，不能把保活当无限运行。[官方前台服务超时说明](https://developer.android.com/develop/background-work/services/fgs/timeout)

组件进程异常退出、外壳重新启动或回前台后，**无条件 `dl.list()` 对账**。事件日志只有 2000 条，外壳冻结期间可能丢增量，seq 也会随进程重启归零，不能用事件历史重建权威状态。配置状态目录后，waiting/active 重启为 waiting，paused 保持 paused，error（含 cancelled）保持 error；失败重试调 `dl.resume`，不要再次 enqueue 同 id。外壳不希望自动流量时，启动对账后暂停恢复的任务。macOS 既有启动对账策略保持。

`dest_dir` 必须是组件能按路径写入的目录，例如 `filesDir` 或 `getExternalFilesDir(...)`。`content://` URI 不能直接传入 dest_dir。公共相册/Downloads 用外壳的 MediaStore/SAF 导出，建议复制并保留组件路径；移动后旧路径不再对应文件，不能继续按旧路径做幂等/续传判断。[应用专属存储说明](https://developer.android.com/training/data-storage/app-specific)

`dl.prune` 仍只清内存终态快照，不删除完成记录；同 job_id 的完成幂等不变。

## 7. cdylib / JNI 备选

将 `libxspider.so` 放入 jniLibs。它是 C ABI，**不含 JNI 方法**：外壳需写薄 JNI 适配或自己选定直接 C ABI 调用桥。JNI 只负责 UTF-8 转换、调用与 `xspider_free`，不重写 HTTP、签名、限流、下载逻辑。

返回的 char* 是标准 UTF-8；JNI `NewStringUTF` 使用 modified UTF-8，不能直接拿它转换可能包含 emoji 的 JSON。JNI 参数也用 `params.toByteArray(Charsets.UTF_8)` 传 byte[]，native 复制并加结尾 NUL，不用 GetStringUTFChars 处理 JSON。结果复制为 byte[] 后用 Kotlin `String(bytes, Charsets.UTF_8)` 解码，再 `xspider_free`。

`xspider_call` 是同步阻塞、进程级单例。调用放 IO 线程，不能在 UI 线程执行或在组件 Tokio 运行时内再次嵌套 block_on。没有 C ABI 取消/销毁/配置导出，也没有 subscribe 推送入口。cdylib 配置需由 JNI 在**第一次任何调用前**单次 `setenv`，且不得与已在读环境的其他线程竞态；Java 没有公共进程环境写 API。需要无需 native 环境配置、每请求取消或独立重启生命周期时使用本册 sidecar 路径。不要期待取消 Kotlin coroutine 能中断同步 native 调用。

APK 内组件更新 = 更新 APK，不能照抄 macOS 的“外部可写目录替换可执行文件”方案；两种形态都在启动握手检查版本及 method。

## 8. 本轮验证记录

2026-10-05，本轮最终源码经独立审阅，已修正过期派发、快速暂停/恢复的文件竞争和取消清理失败后的重试路径。记录恢复测试包含旧 version 1、Paused、Active、取消、Error 和写盘失败场景。

| 验证 | 实际结果 |
|---|---|
| macOS 回归 | workspace 离线 test、clippy `-D warnings`、fmt、双形态/CLI 离线 smoke 通过；release 包构建及 ad-hoc 签名检查通过 |
| Android 构建 | rustc 1.98.1、NDK 27.3.13750724、API 23，`RUSTFLAGS='-D warnings' ./script/android-build.sh arm64-v8a x86_64` 两个 ABI 实际链接并打包通过 |
| ELF/ABI | 两个 ABI 的 sidecar/CDylib 为对应 DYN；sidecar 有 PIE 标记，所有 LOAD 对齐 `0x4000`；三个 C ABI 导出齐全 |
| APK/执行 | arm64 模拟器 `emulator-5554`，API 36；测试 APK targetSdk 35、普通应用 UID 10222，从安装后的 nativeLibraryDir 执行成功 |
| 主形态 | Java HTTP 客户端成功完成 ready/token 握手、system.version、真实 fixture 回放取用户、本地 HTTP 下载；32,791 字节逐字节一致，随后正常 shutdown |
| 备选传输 | stdio fixture 调用通过；C 程序 dlopen Android cdylib 并调用/释放三个 C ABI 接口通过。此项不是 JNI/Kotlin 桥验收 |
| TLS | `XSPIDER_LIVE=1 XSPIDER_ANDROID_PROXY=http://127.0.0.1:17890 ./script/android-smoke.sh` 经 adb reverse 和宿主代理访问公开 `https://x.com/robots.txt`，无账号凭据；probe 响应成功、`size:null`，只证明 TLS/HTTP 传输完成，服务端未给长度 |
| 16 KB | ELF LOAD 与 APK `zipalign -P 16` 检查通过；模拟器实际页大小 4096，未在 16 KB 页设备实跑 |
| 打包与清理 | 只复制 Git 跟踪的 14 个 fixture 文件，排除 raw/.tmp；测试 APK 卸载，测试创建的 adb reverse 映射清理，既有映射不替换 |
| CI | Android 两 ABI 构建 job 已添加，本轮未运行 GitHub Actions |

最终包在 `dist/xspiderd-0.1.0-android-{arm64-v8a,x86_64}.tar.gz`，各自附 `.sha256`；其中 `test-fixtures/` 仅用于验证，生产 APK 不携带。

可复跑：先构建对应 ABI，连接设备，再运行 `./script/android-smoke.sh`（默认离线）；公开 HTTPS 需显式设置上表 live 门控。脚本创建无 UI 的临时测试 APK，发现同名已安装应用时拒绝覆盖。

**仍未验收**：X 账号 GraphQL live、x86_64 设备运行、API 23 实机、真实 16 KB 设备、前台服务/Doze/phantom process 回收、MediaStore 导出、Aria2Next Android APK 后端及用户 CA。组件侧重启恢复已有离线 E2E，不能代替实际产品的后台生命周期验收。
