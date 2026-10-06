# 05 · 开发流程、里程碑与工程陷阱

---

## 1. 每轮工作循环（固定套路，不要跳）

1. 维护者在本地先读仓库根目录的 `AGENTS.md`（仅本地，不随仓库发布；其踩坑记录与常用命令已于
   2026-10-06 迁入专题文档——总索引见本册 §8，命令手册见本册 §9）；外部贡献者读本册铁律与相关专题文档；
2. **选一个可验证的目标**（一句话能说清"怎么算做完"）；
3. **先写测试**（fixture 或断言先落地），再写实现；
4. **实现到测试通过**，期间不顺手重构无关代码；
5. **跑质量门**：`fmt` / `clippy -D warnings` / `test`（离线）；
6. **更新文档**：契约变了改 `CONTRACT.md`；决策变了写 ADR；踩坑当天写进对应主题文档
   （02 协议 / 03 构建 / 04 测试 / 05 工作流 / 12 运行时 / history mac 专项），并在本册 §8 总索引登记；
7. **汇报**（五段式，见 §5）。

**每轮结束时代码必须是可构建、可测试的状态**，不要留半成品在主干。

---

## 2. 里程碑与验收标准

| 里程碑 | 内容 | 验收标准（可验证） |
|---|---|---|
| **M0 地基** | workspace 骨架、`CONTRACT.md` + JSON Schema、`DECISIONS.md`、`ROADMAP.md`、垂直切片 | ① `xspider_version` 在 cdylib 与 sidecar 两形态均可调用；② `fetch.get_user` 端到端跑通（离线 fixture + live 各一次）；③ `script/smoke.sh` 一条命令验证完；④ sidecar `--port 0` 打印 ready 行 |
| **M1 取数** | 3–5 个端点（user / user_medias / user_tweets / tweet_detail / search_timeline）、限流闸门与熔断、内置 HTTP 下载后端 | ① 每端点 ≥5 类 fixture 样本全绿；② 分页语义测试（游标省略/推进/空页/游标未推进）全绿；③ 429 熔断与恢复有测试；④ canary 能报出"哪个端点哪个字段" |
| **M2 爬取与下载** | 候选清单 + 策略参数 + `done_reason`、下载队列与状态机、`http` 后端、aria2 外派后端、完整性校验 | ① 本地 HTTP E2E 全绿（含断点续传/断流重试/完整性失败）；② `job_id` 幂等与重启恢复有测试；③ 引擎选择按 `requirements` 生效；④ 下载记录由组件写、格式有版本字段 |
| **M3 分发** | 打包脚本、签名、NOTICE/LICENSE、README；平台验收按 ADR-038 与 ADR-042 | ① macOS `build+test+clippy`、cdylib dlopen 与 sidecar 冒烟通过；② Android 增量 `cargo check` 只验证可编译，设备/live 验收单独记录；③ 产物结构见 `03` §5/§6 |
| **M4 真实消费方** | 先用 CLI 当第一个真实调用方，再评估接入 `x-spider-mac` | ① CLI 能完成"取一页 → 下 3 个媒体 → 报告结果"；② 契约在真实使用中未被推翻（若有变更，记为 ADR）；③ 记录"实际接入所需的改动清单" |

**工期量级**：M0 3–5 天；M1 2–3 周；M2 2–3 周；M3 1 周；M4 1–2 周（业余节奏）。
**两周内能见效的最小目标 = M0。**

---

## 3. ADR 纪律

什么时候必须写 ADR（`docs/DECISIONS.md`）：
- 定义或修改对外契约；
- 选依赖（HTTP 栈、异步运行时、序列化、下载引擎）；
- 改变组件边界（什么进组件、什么留外壳）；
- 推翻本文档里的既有结论。

格式（每条不超过 15 行）：

```markdown
## ADR-EXAMPLE 下载任务归属组件，而非外派给外壳
- 背景：外壳各自实现下载会导致断点续传/完整性校验重复且不一致
- 选项：A 组件持有 / B 组件只产任务描述、外壳执行 / C 两者都支持但只保留 A 为主
- 决定：A 为主；B 仅作为待设计的平台适配方向，不代表已有 method 或接口形状
- 理由：<…>   代价：<…>   何时该推翻：<…>
```

---

## 4. 提交与代码纪律

- 提交信息写**为什么**，不写"update file"；一个提交一个可验证目标。
- 不提交生成物（`target/`、`dist/`）；`.gitignore` 先写好。
- 依赖纪律：能不加就不加；加之前看许可证（**GPL 兼容性**）与维护状态；
  建议上 `cargo deny` 或至少 `cargo audit`。
- 锁定工具链（`rust-toolchain.toml` + `Cargo.lock` 入库），保证换机器可复现。
- 日志：结构化 + 分模块；**绝不打印 cookie / token / 完整请求头**。

---

## 5. 汇报格式（五段，不要流水账）

1. **本轮产出**：文件路径 + 一句话；
2. **实测证据**：跑过的命令 + 关键输出（尤其是失败与边界，别只贴成功）；
3. **决策与理由**：新增/变更的 ADR；
4. **未决与风险**：卡在哪、需要谁定什么；
5. **下一步**：下一轮的验收物。

---

## 6. 工程陷阱清单（会真的咬人）

本节按主题给出通用守则；**带编号的条目**（N. 标题）迁自 AGENTS.md「踩坑记录」（2026-10-06），
编号与 §8 踩坑总索引一致，按「现象 → 根因 → 解法（含证据）」完整保留实测细节。

**sidecar / 进程**
- 端口占用与僵尸进程：退出路径要覆盖 panic / SIGTERM / 外壳崩溃；冒烟脚本必须断言"无残留"。
- 固定端口在多实例下必炸：用 `--port 0` + ready 行，或每实例独立端口 + 独立 state-dir。
- 子进程（aria2）必须跟随父进程退出（进程组/`kill_on_drop`/显式清理三选一并测）。

**网络与下载**
- 大文件不要整块读进内存：流式写盘 + 分片。
- 写入必须原子（临时文件 + rename），跨卷 rename 会失败，要回退到"复制 + fsync + 删除"。
- 磁盘满、只读目录、路径过长（Windows 260 字符）、非法字符（Windows）都要有明确错误码。
- 时间与超时：统一用单调时钟；区分"请求超时"与"整体任务超时"。

**8. 代理端口会变，而且会整段时间不可达——所以 `net.set_proxy` 是必需品。**

- 现象：同一天内观察到的代理端口：`17890` → 不可达 → `12450` → 不可达 → `17890`。
  代理不可达时，x.com **直连也不通**（`curl` 立即 000），于是 live 测试会以
  `transport{kind:"connect"}` 失败。
- 解法：① 契约里加 `net.set_proxy`（ADR-007），运行时就能换，不必重启进程；
  ② `script/smoke.sh` 支持 `XSPIDER_PROXY`；
  ③ **分清"环境问题"与"代码问题"**：`kind=connect` 且错误 URL 是探测页地址时，
  先 `curl -x $XSPIDER_PROXY https://x.com/robots.txt` 确认代理，再怀疑代码。

**21. 抓公开资源的辅助函数也需要重试。**

- 现象：queryId 自愈偶发失败（`transport{kind:"connect"}`），而同一个 URL 前一次是成功的。
- 根因：`fetch_text_with` 是单发请求，没有像 `send` 那样的重试预算；本机代理会瞬时抖动。
- 解法：给它加上"只重试传输层失败"的 3 次退避（拿到 4xx/5xx 不重试）。
  教训：**关键路径上的每一条出网路径都要有重试策略**，不能只有主路径有。

**Rust 特有**
- 不要在有 async 的地方做阻塞 IO（用 `spawn_blocking`）。
- `Option<String>` 序列化要 `skip_serializing_if = "Option::is_none"`（见 `docs/02-X-DOMAIN-NOTES.md` A1）。
- 泛型/结构体布局**不要**出现在 C ABI 边界。
- 错误里带上**上下文**（端点、页、字段名），否则 X 改版时你只看得到"解析失败"。

**10. `impl Fn(..) -> Pin<Box<dyn Future + Send>>` 的闭包要求 `'static`，不能借用外层变量。**

- 现象：想给签名加载器传一个闭包，闭包里要捕获 `&CancelToken` 与 `&HttpStack`，
  编译不过（`Box<dyn Future>` 默认要求 `'static`）。
- 解法：签名改成 `Arc<dyn Fn(..) -> Pin<Box<dyn Future + Send>> + Send + Sync>`，
  闭包内部 **clone** 需要的东西。见 `xclid::FetchFn`。

**20. reqwest 0.12 的 `RequestBuilder` 没有 per-request 重定向策略。**

- 现象：想让"公开页面跟随重定向、`/i/api/` 不跟随"，`builder.redirect(...)` 编译不过。
- 解法：`ReqwestTransport` 持有两个 client（`client` 严格 / `web_client` 跟随），
  按请求里的 `follow_redirects` 选。**不跟随是刻意的防线**：跟随会把鉴权失败伪装成成功。

**macOS 特有**
- 未签名 / 被 quarantine 的二进制会被 SIGKILL（137），表现为"静默失灵"——启动自检要拦下来。
- 一旦启用 hardened runtime（公证前提），`dlopen` 任何 dylib 都会被拒（实测见 `03`）；
  sidecar 形态没有这个问题。

**脚本与 shell**

**2. `set -u` + macOS 自带 bash 3.2：展开空数组会炸。**

- 现象：`"${BUILD_FLAGS[@]}"` 在数组为空时报 `unbound variable`。
- 根因：bash 3.2 在 `set -u` 下把空数组展开视为未绑定（4.4 才修）。
- 解法：不用数组拼参数，改成函数分支（`build_pkg`）。见 `script/smoke.sh`。

**3. `$VAR` 后面紧跟多字节字符时，bash 会把多字节字节算进变量名。**

- 现象：`"构建完成（$PROFILE）"` 报 `PROFILE\xef: unbound variable`。
- 根因：bash 3.2 在 UTF-8 locale 下把高位字节当成标识符字符。
- 解法：一律写 `${PROFILE}`。**写中文提示文案时尤其要注意**（本仓库的脚本全是中文提示）。

**依赖与构建**
- `native-tls` 会让交叉编译 Windows/Linux 变痛苦 → 用 `rustls-tls`。
- 生成绑定（若用 BoltFFI/UniFFI）必须**锁定精确版本**，它的 0.x 破坏性更新会波及外壳。

---

## 7. 文档即产品

这个仓库的消费者是"别的平台的外壳 + 未来的 agent"，所以：
- `CONTRACT.md` 的清晰度直接决定别人能不能用；
- 踩坑记录直接决定下一轮维护者会不会重踩——它们已从本地操作手册（AGENTS.md）迁入公开的
  专题文档，按主题归档，以本册 §8 踩坑总索引为统一入口；
- 每完成一个里程碑，回看一遍文档是否与代码一致——**不一致的文档比没有文档更坏**。

---

## §8 踩坑总索引（45 条，2026-10-06 迁出自 AGENTS.md）

编号沿用 AGENTS.md「踩坑记录」的原始编号；各条正文（现象 → 根因 → 解法，含全部实测证据）
已迁入「所在文档」列对应的主题文档，其中 2、3、8、10、20、21 六条在本册 §6。
文档号对应 `docs/` 下同编号文档（12 = 下载与运行时笔记）；history 为 mac 接入专项归档，
两条均注明了具体文件。**新踩的坑当天写进对应主题文档，并在本表追加一行。**

| 坑号 | 一句话标题 | 所在文档 |
|---|---|---|
| 1 | rust-toolchain 写死版本会把工具链装成半成品 | docs/03 |
| 2 | bash 3.2 的 set -u 下展开空数组会炸 | docs/05 |
| 3 | $VAR 后跟多字节字符被算进变量名 | docs/05 |
| 4 | 403 空 body 与 200 data={} 的错误分类 | docs/02 |
| 5 | HTTP-date 与 X 时间格式要两套解析 | docs/02 |
| 6 | OnceLock 引擎不能让并行测试共享 | docs/04 |
| 7 | 回放模式不能走签名加载 | docs/04 |
| 8 | 代理端口会变，net.set_proxy 必需 | docs/05 |
| 9 | cdylib 产物名默认取 crate 名 | docs/03 |
| 10 | Fn 闭包要求 'static，捕获需 clone | docs/05 |
| 11 | 响应自带 x-rate-limit-* 头 | docs/02 |
| 12 | serde_json 默认不保序会让 fixture 失真 | docs/04 |
| 13 | 脱敏脚本必须自带事后断言 | docs/04 |
| 14 | 同一端点会返回两种用户结构 | docs/02 |
| 15 | 码率变体键名实测是 content_type | docs/02 |
| 16 | 置顶推文在 TimelinePinEntry 指令里 | docs/02 |
| 17 | 假 id 冲突会让去重毁掉整页 | docs/04 |
| 18 | 通用键 value 不能盲目脱敏（毁游标） | docs/04 |
| 19 | /search 页 queryId 自愈必须带凭据 | docs/02 |
| 20 | reqwest 无 per-request 重定向策略 | docs/05 |
| 21 | 公开资源的辅助请求也要重试预算 | docs/05 |
| 22 | Aria2Next 对 404 报成功并留 0 字节文件 | docs/12 |
| 23 | Aria2Next RPC 错误全 code:1 且 --help 不全 | docs/12 |
| 24 | 两引擎断点靠文件名物理隔离 | docs/12 |
| 25 | 并发断言断语义不断 socket 巧合 | docs/04 |
| 26 | 过期结果会覆盖新状态：每次派发要有 epoch | docs/12 |
| 27 | 取消不能提前把状态标成终态 | docs/12 |
| 28 | 下载记录必须存 url 与 expect_size | docs/12 |
| 29 | 先探测再绑定的端口分配有竞态 | docs/12 |
| 30 | 媒体大小只能问 CDN，码率×时长差 5 倍 | docs/02 |
| 31 | 队列下载前探测补完整性校验 | docs/12 |
| 32 | 下载请求要带 UA/Referer 与 identity | docs/12 |
| 33 | aria2 代理要逐任务显式传 | docs/12 |
| 34 | 运行中换代理要全路径跟上 | docs/12 |
| 35 | 长推文正文在 note_tweet.note_text | docs/02 |
| 36 | 组件必须自带父进程看门狗 | docs/12 |
| 37 | 删实现先过一遍资源管理代码 | docs/history/mac-integration-2026-10/MIGRATION.md |
| 38 | 写操作成败看返回体不看状态码 | docs/02 |
| 39 | 写测试别拿可能存在的对象当不存在 | docs/04 |
| 40 | already_known 是幂等命中，恢复用 dl.resume | docs/12 |
| 41 | 对照参考实现要对它给外壳的模型 | docs/02 |
| 42 | Swift 同句读写本地 var 触发独占性崩溃 | docs/history/mac-integration-2026-10/MIGRATION.md |
| 43 | 交叉编译 E0463 多为 PATH 拿错 rustc | docs/03 |
| 44 | dispatch 必须校验调用方那一轮 epoch | docs/12 |
| 45 | Android 打包验收用普通 UID 与当前 targetSdk | docs/03 |

---

## §9 命令手册（迁入自 AGENTS.md）

> 维护约定：命令有变直接改本节；`AGENTS.md` 不再保留副本，只留指路。

**环境初始化**（一次性；迁自本地启动简报 §6，该简报不入库，其余文档此前未收录）：

```bash
# 工具链（仓库当前用 stable，并在 rust-toolchain.toml 注释实测版本；原因见 ADR-011）
rustup toolchain install stable
rustup component add rustfmt clippy
cargo install cargo-nextest   # 可选，但测试体验明显更好
```

> 本机 PATH 里排在前面的是 **Homebrew 的 cargo**，它是真实二进制、**不读 `rust-toolchain.toml`**。
> 要用仓库锁定的工具链，显式用 `~/.cargo/bin/cargo`（rustup 垫片）。下面的命令都按这个来。

```bash
CARGO=~/.cargo/bin/cargo

# —— 质量门（提交前必须三条全绿）——
$CARGO test --workspace --offline                                  # 默认离线，不碰网络（铁律 3）
$CARGO clippy --workspace --all-targets --offline -- -D warnings
$CARGO fmt --all --check

# —— 下载后端（离线 E2E，用本地 HTTP fixture server）——
$CARGO test -p xspider-download --offline            # 下载组件单元与本地 HTTP / Aria2Next E2E；以本次输出为准
# Aria2Next E2E 需要那个二进制：XSPIDER_ARIA2_PATH 指定，或用本机随 x-spider-mac 带的那个。
# 找不到就**大声跳过**（不是静默通过）——它覆盖的是"引擎报成功其实失败"这类骗人行为。
$CARGO test -p xspider-download --offline --test aria2_e2e

# —— 垂直切片：一条命令验证双形态 + 端到端 ——
./script/smoke.sh                     # 离线：fixtures 回放，含 cdylib dlopen + sidecar 握手/调用/关停/无残留 + CLI 只经契约跑一遍
PROFILE=release ./script/smoke.sh     # 同上，release 构建

# —— 真实消费方（CLI）：**不链接任何本仓库 crate**，只经契约 ——
$CARGO run -p xspider-cli --offline -- --screen-name demo_user --fixture-dir fixtures --dry-run
$CARGO run -p xspider-cli --offline -- --list-methods --fixture-dir fixtures   # 外壳的启动自检
$CARGO run -p xspider-cli --offline -- --screen-name demo_user --fixture-dir fixtures --dry-run --json  # 报告给机器读
# live（会真的下载，注意体积；--count 控制个数）
$CARGO run -p xspider-cli --offline -- --screen-name tesla --count 3 --out ./downloads \
  --proxy "$XSPIDER_PROXY" --segments 4

# —— live（会消耗账号配额，默认不跑）——
export XSPIDER_LIVE=1
export XSPIDER_COOKIE="$(defaults read moe.keli.xspider.mac app.cookieString)"   # 只进不出，别写文件
export XSPIDER_PROXY=http://127.0.0.1:17890                                      # 访问 x.com 需要（端口会变，见踩坑 8）
XSPIDER_SMOKE_SCREEN_NAME=tesla ./script/smoke.sh          # live 冒烟
$CARGO test -p xspider-fetch --test canary_live --offline -- --ignored --nocapture   # live canary（"X 又变了"报警器）
$CARGO test -p xspider-fetch --test record_live --offline -- --ignored --nocapture   # 录 fixture（原始响应落 raw/）
$CARGO test -p xspider-core  --lib --offline -- --ignored --nocapture record_xclid_page  # 录 xclid 页面原料
python3 script/redact_fixtures.py                                  # raw/ → 可入库的脱敏 fixture（自带 4 条自检）

# —— sidecar 手动调试 ——
$CARGO run -p xspiderd -- --port 0              # 绑定随机端口，stdout 打印一行 ready {...}
$CARGO run -p xspiderd -- --port 0 --fixture-dir fixtures   # 离线回放（测试用）
$CARGO run -p xspiderd -- --stdio               # stdin/stdout 的 JSON Lines 模式
$CARGO run -p xspiderd -- --help
XSPIDER_LOG=debug $CARGO run -p xspiderd -- --port 0        # 日志走 stderr，看得到每一步

# 用 curl 直接调（ready 行里读 port 与 token）
curl -s "http://127.0.0.1:$PORT/" -H "X-XSpider-Token: $TOKEN" -H 'Content-Type: application/json' \
  -d '{"method":"fetch.get_user","params":{"screen_name":"demo_user"}}'

# —— cdylib 形态单独验证（冒烟脚本已包含）——
cc -o /tmp/cdylib_check script/cdylib_check.c && /tmp/cdylib_check target/debug/libxspider.dylib
```

**离线构建**：`--offline` 让 cargo 只用本地缓存。依赖已全部缓存在本机时更快，也更稳
（本机代理时通时断，见踩坑 8）。首次或改了依赖才需要联网 `cargo fetch`。
