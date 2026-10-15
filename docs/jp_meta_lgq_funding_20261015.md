# jp-meta lgq 三个 CTA RapidX funding — 2026-10-15

按操作员要求，为 lgq 三个 CTA 添加 RapidX 实际资金费采集，并特别允许
固定使用账户交易 IP。最终发布标识 `20261015T030235Z`，基线 `0502f40`，
同时包含 [RX01 成交读取修复](jp_meta_rx01_fill_recovery_20261015.md)。

| source ID | 账户 | 已核对账单 | 历史资金费，USDT |
| --- | --- | ---: | ---: |
| binance_exec_trade05 | lgq_cta_RX02 | 108 | 0.1288908844089895186 |
| binance_exec_trade06 | lgq_cta_RX01 | 75 | -0.5858939310806462822 |
| binance_exec_trade07 | lgq_cta_RX03 | 14 | -0.0194278629221733840 |

上述是首次完整核对的固定范围，后续按 300 秒间隔继续采集。三个账户状态
均为 `ok`，没有采集错误；其他四个账户保持关闭。RX02/RX01 分别观察
16 个永续币种，RX03 观察 14 个。起点沿用各币种最早本地订单创建时间，
独立于功能启用时间及仓位快照；已有历史不会重命名为新 source。

## 出口与账单契约

实测三个账户当前交易绑定均为 `172.31.35.228`，公网地址
`13.115.227.29`。Manager TOML 新增 `[funding]`，明确配置这一固定
local/public 对，并在 `rapidx_trading_ip_sources` 列出这三个完整
source ID。每轮读取本账户实际交易配置，要求固定 IP 完全匹配，
禁用代理和重定向，并验证公网地址。绑定变化时停止采集；没有默认路由
或其他 IP 回退。该例外仅适用于明确列出的 RapidX Binance USD-M 来源，
原生账户及其他来源继续使用原专用非交易出口规则。

根据现有 RapidX 实现及官方 [Query Statement](https://apidocliquidity.readme.io/reference/query-statement)
接入 `FUNDING_FEE` 账单。验证 portfolio、BINANCE/PERP、完整符号、币种、
时间、页码、页数和总数；使用 `statementId` 去重，`deltaAmount` 原始
有符号十进制入库。分页不完整、范围或身份不符时不推进 cursor。RapidX
三账户共享每滚动分钟 40 次请求预算；原生 income 的预算保持原值。

PostgreSQL 使用显式维护事务添加三个 funding 账单/进度表和五个账户设置
字段，所有账户默认关闭。没有启动 DDL 或自动 migration。启用 token
仅保存于本机 0600 环境文件；通过临时管理员会话逐账户执行固定出口及
签名 REST 测试，成功后由配置 API 保存开关，测试会话均在 finally 删除。

## 验证与运行范围

独立使用运行中各 trade engine 的凭据、显式绑定当前交易 IP，再次从
RapidX 拉取同一完整范围的账单，与 PostgreSQL 的 statement ID、symbol、
asset、时间及精确十进制金额逐项比较，三账户全部一致。

账户 `/account-timeline` 验证“订单 NAV + 区间实际 funding = 综合 NAV”，
扣费前后均一致；资金费曲线起点为零、终点等于区间金额。策略 `/timeline`
不包含 funding，没有将资金费分配给 CTA 策略或改变订单费用公式。
Manager 账户配置、净值页及工作区展示资金费状态和综合 PnL。

隔离发布包通过 `cargo fmt --check`、`cargo check --locked`、完整
`cargo test --locked`（316 个库测试、9 个快照工具测试通过；4 个库测试和
7 个 PostgreSQL 集成测试按现有配置忽略）、release 构建及前端 build/lint。
lint 的两个未使用变量/导入提示为原有代码。新增测试覆盖账单范围、币种、
符号、完整分页、签名及交易 IP 例外隔离。

只替换 `cta_web`、`nav_rebuild` 及 Manager 前端，只重启 Manager 服务。
保留 CPU 24 绑定；187 个受保护进程的 PID/启动标识一致；30 份运行配置
仅 Manager TOML 增加 funding 段，原字段与其余配置散列一致。四张账户/
策略仓位快照表完整内容散列不变。Exec RocksDB 全程只读，未更改交易
绑定、目标、持久化状态或 Nginx 配置。

最终 Manager health 为 `ok`、`last_refresh_error=null`；前端全部文件通过
网关下载的散列匹配发布包，三个账户的 Viz 首页、snapshot、Config 共
9 条网关路径返回 200。目标历史索引完成，`ready=true`、`loading=false`、
`last_error=null`，处理计数 4,720,808。

证据、复核脚本、原快照及发布 manifest 保存于
`artifacts/jp_meta_rx01_initial_snapshot_20261015/`。没有包含 API 密钥、
启用 token 或数据库凭据。

45 份远端证据文件复制到本地后逐一核对 SHA-256，临时远端备份/探测文件
按精确路径删除；临时源码目录也已删除，保留发布补丁、manifest、二进制
和前端归档。正式 release、运行配置及本机凭据文件保留。
