# 09 · 接口清单（26 个 method）

> 这是**入口索引**：26 个契约 method 逐个给出「作用」与「只读 / 写操作」，每行链到
> [`07-API-REFERENCE.md`](07-API-REFERENCE.md) 里的详细小节（入参、出参、错误、调用时序）。
>
> 名称与数量与 [`crates/xspider-ffi/src/engine.rs`](../crates/xspider-ffi/src/engine.rs)
> 的 `METHODS` 派发表、以及机器可读契约
> [`contract/xspider.schema.json`](../contract/xspider.schema.json) 的 `method` 枚举**逐字一致**
> （由契约守卫测试盯着，`fetch.nope` 那类测试专用名不在其中）。
>
> 另有 1 个**传输层** method `system.shutdown`（仅 sidecar，优雅退出）——它不在上面 26 个里，
> 也不出现在 `system.methods` 的返回中。

**「读写」列的含义**：

- **只读**：只读取数据或查询状态，不改变任何东西；
- **写·本地**：改变**组件自己的状态**（凭据、限流/代理参数、下载队列及其落盘文件），不动 X 账号；
- **写·账号**：改变**用户在 X 上的真实账号**（点赞 / 转推 / 书签 / 关注）——只有 `fetch.mutate` 一个。

## 系统（`system.*`，2）

| method | 作用 | 读写 | 详情 |
|---|---|---|---|
| `system.version` | 握手：契约版本、构建版本、当前形态（`sidecar` / `cdylib`） | 只读 | [§4.1](07-API-REFERENCE.md#methods-system) |
| `system.methods` | 能力自检：列出本版本支持的全部契约 method | 只读 | [§4.1](07-API-REFERENCE.md#methods-system) |

## 凭据与网络（`auth.*` 2 + `net.*` 4）

| method | 作用 | 读写 | 详情 |
|---|---|---|---|
| `auth.set_cookie` | 注入凭据（只进不出，不回显） | 写·本地 | [§4.2](07-API-REFERENCE.md#methods-auth-network) |
| `auth.whoami` | 登录校验 / 当前账号 | 只读 | [§4.2](07-API-REFERENCE.md#methods-auth-network) |
| `net.set_limits` | 设限流参数（接口速率 / 突发 / CDN 并发 / 冷却） | 写·本地 | [§4.2](07-API-REFERENCE.md#methods-auth-network) |
| `net.set_proxy` | 运行中换 / 关代理（取数与下载一起换） | 写·本地 | [§4.2](07-API-REFERENCE.md#methods-auth-network) |
| `net.status` | 限流状态（是否处于冷却） | 只读 | [§4.2](07-API-REFERENCE.md#methods-auth-network) |
| `net.probe_size` | 问 CDN 这个文件多大（一次至多 1 字节流量） | 只读 | [§4.2](07-API-REFERENCE.md#methods-auth-network) |

## 取数（`fetch.*`，9）

| method | 作用 | 读写 | 详情 |
|---|---|---|---|
| `fetch.get_user` | 用户资料 | 只读 | [§4.3](07-API-REFERENCE.md#methods-fetch) |
| `fetch.user_medias` | 媒体时间线（单页 + 游标） | 只读 | [§4.3](07-API-REFERENCE.md#methods-fetch) |
| `fetch.user_tweets` | 推文时间线（单页 + 游标） | 只读 | [§4.3](07-API-REFERENCE.md#methods-fetch) |
| `fetch.tweet_detail` | 推文详情 + 回复列表 | 只读 | [§4.3](07-API-REFERENCE.md#methods-fetch) |
| `fetch.search_timeline` | 按日期搜某人的推文 | 只读 | [§4.3](07-API-REFERENCE.md#methods-fetch) |
| `fetch.home_timeline` | 主页时间线（推荐 / 关注） | 只读 | [§4.3](07-API-REFERENCE.md#methods-fetch) |
| `fetch.following` | 关注列表 | 只读 | [§4.3](07-API-REFERENCE.md#methods-fetch) |
| `fetch.is_following` | 关注态（每张推文卡都会问） | 只读 | [§4.3](07-API-REFERENCE.md#methods-fetch) |
| `fetch.mutate` | **写操作**：点赞 / 转推 / 书签 / 关注（8 种 `action`） | 写·账号 | [§4.3](07-API-REFERENCE.md#methods-fetch) |

## 下载（`dl.*`，8）

| method | 作用 | 读写 | 详情 |
|---|---|---|---|
| `dl.enqueue` | 入队下载（`job_id` 幂等） | 写·本地 | [§4.4](07-API-REFERENCE.md#methods-download) |
| `dl.pause` | 暂停任务（**保留**断点） | 写·本地 | [§4.4](07-API-REFERENCE.md#methods-download) |
| `dl.resume` | 恢复任务（对 `paused` 与 `error` 都有效） | 写·本地 | [§4.4](07-API-REFERENCE.md#methods-download) |
| `dl.cancel` | 取消任务（**丢弃**断点） | 写·本地 | [§4.4](07-API-REFERENCE.md#methods-download) |
| `dl.status` | 单个任务快照 | 只读 | [§4.4](07-API-REFERENCE.md#methods-download) |
| `dl.list` | 全部任务快照（重启对账的权威来源） | 只读 | [§4.4](07-API-REFERENCE.md#methods-download) |
| `dl.events` | 增量事件（带游标的轮询，不是推送流） | 只读 | [§4.4](07-API-REFERENCE.md#methods-download) |
| `dl.prune` | 清掉已结束的任务快照（只清内存，不清下载记录） | 写·本地 | [§4.4](07-API-REFERENCE.md#methods-download) |

## 爬取（`crawl.*`，1）

| method | 作用 | 读写 | 详情 |
|---|---|---|---|
| `crawl.run` | 跑一轮爬取，产出候选清单与完整推文（不改任何状态） | 只读 | [§4.5](07-API-REFERENCE.md#methods-crawl) |

---

## 合计

`system.*` 2 + `auth.*` 2 + `net.*` 4 + `fetch.*` 9 + `dl.*` 8 + `crawl.run` 1 = **26**。

对象形状（`post` / `media` / `page` / `reply` / `job` / `downloadEvent` / `candidate`）见
[`07-API-REFERENCE.md`](07-API-REFERENCE.md) §5；错误码与重试纪律见其 §6；
可直接照抄的调用时序见其 §7。
