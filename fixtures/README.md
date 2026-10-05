# fixtures —— 本仓库最重要的测试资产

> HTTP fixture 必须来自真实响应并经过脱敏，不许手写 JSON 冒充。签名测试另有从真实页面
> 提取的 `xclid/page_artifacts.json` 原料；它不是 HTTP 响应 fixture。

## 当前目录与覆盖

```text
fixtures/
├── user_by_screen_name/
│   ├── normal.json
│   ├── not_found.json
│   └── unauthorized.json
├── user_medias/                 # page1.json、page2.json（首页与翻页）
├── user_tweets/                 # page1.json
├── tweet_detail/                # with_replies.json
├── search_timeline/             # media_only.json、query_id_source.json
├── home_timeline/               # for_you.json、following.json
├── following/                   # page1.json
└── xclid/
    └── page_artifacts.json      # 页面签名原料，不是 HTTP fixture
```

`search_timeline/query_id_source.json` 保存真实 bundle 中锚定到 `SearchTimeline` 的片段，
用于 queryId 自愈测试；它没有 `response` 字段，因此回放加载器会跳过它。其余端点目录中的
JSON 是脱敏后的真实 HTTP 响应。原始录制只放在各端点的 `raw/` 子目录，不纳入 Git，
回放加载器也会跳过 `raw/`。

| 端点/资产 | 已入库的真实样本 | 覆盖说明 |
|---|---|---|
| `user_by_screen_name` | `normal`、`not_found`、`unauthorized` | 正常、HTTP 200 空 data、匿名请求 HTTP 403 空响应体 |
| `user_medias` | `page1`、`page2` | 首页与带 cursor 的第二页 |
| `user_tweets` | `page1` | 含媒体的推文时间线首页 |
| `tweet_detail` | `with_replies` | 推文详情与回复树 |
| `search_timeline` | `media_only`、`query_id_source` | 搜索响应；另有真实 bundle queryId 原料 |
| `home_timeline` | `for_you`、`following` | 两种 operation 各一页 |
| `following` | `page1` | 关注列表首页 |
| `xclid` | `page_artifacts` | 从真实登录态页面提取的签名密钥/动画原料 |

这份覆盖表描述当前资产，不代表每个端点都具备相同的异常场景。当前没有真实 429 或字段改名
响应；只有确实采集到并脱敏后才增加这些样本。

## 录制与脱敏

录制需要用户主动提供的凭据和可用网络。命令由 `#[ignore]` 测试门控；默认测试不会出网。

```bash
export XSPIDER_LIVE=1
export XSPIDER_COOKIE='auth_token=...; ct0=...'  # 只进不出，不写文件、不打日志
export XSPIDER_PROXY=http://127.0.0.1:17890     # 需要代理时
export XSPIDER_RECORD_SCREEN_NAME=tesla

# 录制用户与时间线端点，raw 写入 fixtures/<endpoint>/raw/<scenario>.json
~/.cargo/bin/cargo test -p xspider-fetch --test record_live --offline -- --ignored --nocapture

# 更新 queryId 的真实 bundle 原料
~/.cargo/bin/cargo test -p xspider-fetch --test record_live --offline -- --ignored --nocapture record_search_query_id_source
~/.cargo/bin/cargo test -p xspider-core --lib --offline -- --ignored --nocapture record_xclid_page

# raw/ → 可入库的脱敏 fixture；脚本含四条事后断言
python3 script/redact_fixtures.py
```

`record_live` 会把抓取响应写入 `fixtures/<endpoint>/raw/`；脱敏脚本按 `endpoint` 与 `scenario`
写回对应端点目录。canary 失败时保存的 `field_changed_<日期>.json` 是未脱敏原料，当前由 canary
直接写在端点目录；要入库前先人工核对并补齐正确的 `match_hint`，再移入 `raw/` 交给脱敏脚本。
不要直接提交 canary 原始文件。

回放使用 sidecar 的 `--fixture-dir fixtures`，或库形态的 `XSPIDER_FIXTURE_DIR=fixtures`。
可用消费方冒烟命令检查离线链路：

```bash
~/.cargo/bin/cargo run -p xspider-cli --offline -- --screen-name demo_user --fixture-dir fixtures --dry-run
```

## 每条 HTTP fixture 的结构

```json
{
  "endpoint": "user_by_screen_name",
  "scenario": "normal",
  "captured_at": "2026-10-01",
  "match": {
    "method": "GET",
    "operation": "UserByScreenName",
    "screen_name": "demo_user",
    "auth": "valid"
  },
  "response": { "status": 200, "headers": { "...": "..." }, "body": { "...": "..." } }
}
```

- `response.body` 保留真实响应结构；个人数据与长数字 id 在脱敏时替换，键序保留以便 diff。
- `match` 是请求路由条件：`auth` 可为 `valid`、`none` 或 `any`；分页端点可用 `cursor: absent` /
  `cursor: present` 区分首页和翻页。
- 路由采用具体度打分：`cursor` 命中 **+8**、`screen_name` **+4**、`operation` **+2**、`auth` **+2**、
  `method` **+1**；条件不匹配的样本不参与，分数最高者获选。没有可解析 cursor 的请求不会命中
  带 cursor 条件的样本。
- `not_found` 与 `unauthorized` 不绑定录制时的一次性用户名；`normal` 会保留脱敏后的
  `screen_name` 条件，避免通配样本意外吞掉正常请求。

## 有意留空的场景

- **真实 429：未录，且有意不通过耗尽账号配额来制造。** 配额响应处理由限流单元测试覆盖；下载
  HTTP E2E 使用本地 server 验证 HTTP 状态处理。这些测试不是 X 响应 fixture。
- **字段改名：未录。** 只有 X 真实改版、canary 留下原始响应后才加入；不手写伪造改版样本。
- **未授权真实响应只在 `user_by_screen_name` 记录。** 目前其余端点没有对应匿名真实样本。

真实响应头中的 `x-rate-limit-limit` / `x-rate-limit-remaining` / `x-rate-limit-reset` 留在 fixture 中，
供排查与后续评估使用；当前契约没有把它们映射到 `net.status`。

## 脱敏纪律

- 不删字段；删字段会降低解析覆盖率。
- 数组裁到最多 3 条，但带 cursor 的条目必须保留；分页游标本身不改。
- 数字 id 按原值映射为互不相同的假数字；任何位置出现的长数字串都要替换。
- 媒体 URL 保留 host、路径替换为假值；cookie、token、事务 id 不得保留。
- 先看脚本自检结果，再 review 输出。原始个人数据、残留长数字串或映射冲突任一出现，都不得入库。
