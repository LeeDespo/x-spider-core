# fixtures —— 本仓库最重要的测试资产

> 铁律（`docs/04-TESTING-AND-FIXTURES.md` §2.1）：**fixture 必须是真实抓到的响应**，
> 不许手写 JSON 冒充。手写的样本只能证明"你按自己以为的格式解析正确"，
> X 改字段时它不会红——那正是「用户先发现问题」的根因。

目录约定：

```
fixtures/
├── README.md                       # 本文件
├── user_by_screen_name/            # fetch.get_user 的真实响应
│   ├── normal.json                 # 正常用户
│   ├── not_found.json              # 用户不存在
│   ├── unauthorized.json           # 未带凭据
│   └── raw/                        # 录制原始响应（.gitignore，不入库）
└── xclid/
    └── page_artifacts.json         # 从真实登录态页面抠出的签名原料
```

## 怎么产生（两步，不要省）

```bash
export XSPIDER_LIVE=1
export XSPIDER_COOKIE='...'                    # 只进不出：不写进文件、不打日志
export XSPIDER_PROXY=http://127.0.0.1:17890    # 需要代理时

# 1. 录制：把**原始**响应落到 <endpoint>/raw/（已 gitignore）
cargo test -p xspider-fetch --test record_live -- --ignored --nocapture
cargo test -p xspider-core  --lib -- --ignored --nocapture record_xclid_page

# 2. 脱敏：raw/ → 可入库的 fixture（脚本自带"原始个人数据不得残留"的自检）
python3 script/redact_fixtures.py
```

为什么分两步：录制保证**真实性**，脱敏脚本保证**不含个人数据**，
而脱敏规则是可审计的（`docs/04` §2.1 那六条一一对应）。合起来才同时满足两个要求。

回放时：`--fixture-dir fixtures`（sidecar）或 `XSPIDER_FIXTURE_DIR=fixtures`（库形态 / 测试）。

## 每条 fixture 的结构

```json
{
  "endpoint": "user_by_screen_name",
  "scenario": "normal",
  "captured_at": "2026-10-01",
  "match": { "method": "GET", "operation": "UserByScreenName", "screen_name": "demo_user", "auth": "valid" },
  "response": { "status": 200, "headers": { ... }, "body": { ... } }
}
```

- `response` 部分**原样保存**上游的响应（body 按原始键序，便于与真实响应直接 diff）；
- `match` 是**回放路由条件**，用来决定"哪个请求命中哪条样本"。
  `auth` 三态：`valid`（必须带凭据）/ `none`（必须不带）/ `any`。
  具体度打分：`screen_name` 命中 +4、`operation` +2、`auth` +2、`method` +1，最高者胜。
  这样 `normal` 会赢过"只约束 operation"的通配样本。
- `not_found` / `unauthorized` **刻意不按 screen_name 匹配**：录制时用的用户名是一次性的
  （一个不存在的用户名 / 未登录），脱敏后没有稳定值可匹配。

## 当前覆盖度

| 场景 | 状态 | 说明 |
|---|---|---|
| 正常 | ✅ 真实 | `normal.json`，HTTP 200 |
| 用户不存在 | ✅ 真实 | `not_found.json`，**HTTP 200 + `{"data":{}}`**——没有 `user`、也没有 `errors[]` |
| 未授权 | ✅ 真实 | `unauthorized.json`，**HTTP 403 且响应体为空** |
| 限流 429 | ❌ **未录，且是有意不录** | 见下 |
| 字段改名（改版） | ❌ 未录 | 只有真的遇到 X 改版时才会产生，不能凭空造 |

### 为什么没有 429 样本

要拿到一条**真实**的 429，必须先把账号打进限流。那是拿使用者的账号配额
去换一条测试数据——**代价不对等**，而且与「定位为个人自用、授权账号范围内的工具」
（AGENTS.md「许可证与合规」）相冲突。

替代方案（按优先级）：

1. **用实测的响应头驱动**：真实响应里 X 会返回
   `x-rate-limit-limit / x-rate-limit-remaining / x-rate-limit-reset`
   （已留档在 `normal.json` 的 headers 里）。M1 可以据此做**主动**限流，
   而不必等 429 出现。
2. 429 的处理逻辑用**单元测试**钉住（`crates/xspider-core/src/ratelimit.rs` 已经覆盖：
   冷却生效、快速失败、退避放大、成功复位、上限封顶、以及"网络异常不留下粘性状态"）。
3. 真到了需要 429 样本的时候，用**本地 HTTP fixture server** 造一个 429 响应
   ——那属于"下载 E2E"层（`docs/04` §5），不是 X 的真实样本，因此**不许**放进本目录
   冒充真实响应。

### 为什么 xclid 只存"原料"而不存整页 HTML

`xclid/page_artifacts.json` 存的是从真实登录态页面**抠出来的**东西：
站点验证密钥（base64 与其解码字节）、4 条 SVG 动画路径、脚本 URL 清单、
签名脚本里的动画索引。它让 `animKey` 的计算能在离线测试里被钉住。

不存整页 HTML 的理由：那是一个 1MB 级、随改版天天变的 minified 大块，
对解析覆盖率的额外价值为零（我们并不解析整页，只抠这几个点），
而它会让 fixture 目录无法 review。这份"原料"是**真实数据的投影**，不是手写样本。

## 纪律

- **不删字段**——删字段等于降低解析覆盖率；
- 数组裁到 3 条以内，但**游标字段原样保留**（它才是分页语义的关键）；
- 媒体 URL 保留 host、路径换成假名；**不要把真实 URL 提交进仓库**；
- 凭据（cookie / token / 事务 id）整段换成 `REDACTED`；
- `raw/` **永远不进仓库**（已在 `.gitignore`）。
