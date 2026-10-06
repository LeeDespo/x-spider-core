# 文档索引

本目录是 x-spider-core 的文档门户：每个主题一份真源。仓库规则与「按任务先读什么」的路由
见根目录 [AGENTS.md](../AGENTS.md)；对外行为的契约见 [CONTRACT.md](CONTRACT.md)。

## 使用组件（写外壳时最常翻）

| 文件 | 内容 |
|---|---|
| [CONTRACT.md](CONTRACT.md) | 对外契约本身：method 表、字段、错误码、版本策略（先改这里，再改代码） |
| [09-METHOD-INDEX.md](09-METHOD-INDEX.md) | 接口入口：26 个 method 的作用与只读/写属性，逐行链到接口参考 |
| [07-API-REFERENCE.md](07-API-REFERENCE.md) | 接口参考：入参/出参、数据形状、错误处理、可直接抄的调用时序 |
| [06-CONSUMER-INTEGRATION.md](06-CONSUMER-INTEGRATION.md) | 接入手册：接入要动哪些代码、消费者视角踩到的坑、未决清单 |

## 理解架构

| 文件 | 内容 |
|---|---|
| [01-ARCHITECTURE.md](01-ARCHITECTURE.md) | 组件边界怎么切、契约形状、下载引擎三种后端、任务归属 |
| [08-CAPABILITY-MAP.md](08-CAPABILITY-MAP.md) | 能力地图：每个契约 method 在哪实现、怎么测、当前缺口、有意不进组件的事 |
| [12-RUNTIME-NOTES.md](12-RUNTIME-NOTES.md) | 运行时注记：下载队列状态机、断点与 epoch、sidecar 生命周期与看门狗 |
| [DECISIONS.md](DECISIONS.md) | 决策台账（ADR）：每条决定与它什么时候该被推翻 |

## 开发与测试

| 文件 | 内容 |
|---|---|
| [02-X-DOMAIN-NOTES.md](02-X-DOMAIN-NOTES.md) | X GraphQL 的领域知识与实测踩坑（照着做能省几周） |
| [03-FFI-SIGNING-PACKAGING.md](03-FFI-SIGNING-PACKAGING.md) | FFI / 签名 / 分发 / 交叉编译的实测结论与踩坑 |
| [04-TESTING-AND-FIXTURES.md](04-TESTING-AND-FIXTURES.md) | 测试与 fixture 纪律、离线默认、live canary |
| [05-WORKFLOW.md](05-WORKFLOW.md) | 工作循环、ADR 纪律、踩坑总索引（§8）与命令手册（§9） |
| [../fixtures/README.md](../fixtures/README.md) | fixture 怎么产生、覆盖度、**哪些缺口是有意留的** |

## 平台

| 文件 | 内容 |
|---|---|
| [10-ANDROID-INTEGRATION.md](10-ANDROID-INTEGRATION.md) | Android 构建、部署、外壳义务与未完成的运行验收 |

## 项目状态与发布

| 文件 | 内容 |
|---|---|
| [ROADMAP.md](ROADMAP.md) | 当前路线图：状态、未完成、平台计划、风险台账 |
| [RELEASING.md](RELEASING.md) | 发布规则（唯一真源）：版本基线、tag 纪律、资产清单、发布义务 |
| [../CHANGELOG.md](../CHANGELOG.md) | 对外可见的变化（只记变化与影响） |

## 历史（默认不读，回归调查 / 考古才查）

| 文件 | 内容 |
|---|---|
| [history/](history/) | 历史归档：审阅记录、mac 接入实录、里程碑验收快照；与活文档冲突时以活文档为准 |
