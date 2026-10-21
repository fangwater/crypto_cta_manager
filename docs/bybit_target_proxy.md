# Binance → Bybit 目标过滤与合约单位换算

Manager 可在指定 Bybit 账户的发布层启用 target proxy。原来的
`POST /api/catalog/position-strategies`、发布 token、策略名、账户绑定和
shares 都继续使用；无需启动额外代理进程，也无需复制一份策略。
未配置 proxy 的账户保留原来的严格市场校验。

## 配置

在 Bybit 的 `[[sources]]` 内添加下面的子表。空表即启用同名过滤：

```toml
[[sources]]
id = "bybit_exec_trade01"
account = "bybit_uta"
venue = "bybit-futures"
rocksdb_path = "/home/ubuntu/bybit_exec_trade01/data/persist_manager"
exec_config_url = "http://127.0.0.1:18161"
ipc_namespace = "bybit_exec_trade01"

[sources.target_proxy]
```

配置从主机 TOML 读取，修改后随 Manager 重启加载。此功能默认不启用。
2026-10-19 已在 SG 的 `bybit_exec_trade01` 启用并接通 RX02，部署版本为
`20261019T132344Z`；Bybit 交易进程仍保持停止。见
[SG 部署记录](sg_deployment.md)。

每个输入先检查是否为 USDT 永续 symbol，再查询本账户的 Bybit 规则缓存。
同名且状态为 `Trading` 的目标保留，默认单位系数为 1；未匹配、停牌、
现货裸资产、USDC、反向及交割合约目标被过滤。过滤后发布的是完整目标向量，
全部被过滤时也发布空向量以替换旧目标，不会沿用上次的目标。
Exec 对原向量中消失的 symbol 使用零目标；不可交易合约仍受 Exec 规则限制。

该缓存复用 Manager 已有的 60 秒全量分页刷新及 Redis 确认流程。
不增加第二套 Redis 格式，也不在收到每个目标时请求交易所。
只有完整校验并成功写入、读回确认的本 source 快照可用于 proxy。
刷新失败保留上一份；未加载、超过 180 秒或时间在未来时返回 HTTP 503，
不把未知缓存视作空列表。普通目标推送在这种情况下不会修改 catalog。
其他账户的缓存不能代替当前账户缓存。

## 合约乘数

异名合约通过显式映射确认同一种底层资产，数量采用：

```text
发布 qty = 输入 qty × source_contract_multiplier / target_contract_multiplier × shares
```

两个 multiplier 都表示「1 个 qty 所包含的同一种底层资产数量」，必须为有限正数。
例如源合约按 PEPE 报数量，目标合约按 1000PEPE 报数量：

```toml
[sources.target_proxy.symbol_mappings.PEPEUSDT]
symbol = "1000PEPEUSDT"
source_contract_multiplier = 1.0
target_contract_multiplier = 1000.0
```

输入 `qty=2000`、`shares=3`，发布 `1000PEPEUSDT qty=6`。
反向换算则配置源 multiplier 为 1000、目标 multiplier 为 1。
示例仅说明配置格式，实际目标必须存在于最新的 Bybit 可交易缓存。
Manager 不通过去掉数字前缀或模糊匹配来猜测底层资产；`1INCH` 等名称按原名处理。
映射不存在的合约仍被过滤；多个输入落到同一个执行 symbol 时拒绝该向量，
不会覆盖或擅自合并。无穷大、NaN、数量溢出和下溢会报错。

`qty: "clear"`、数值零及 signal 保持各自含义。原始 signal 留在归档中，
发布到 Exec 的 signal 仍遵守下单模板的 `signal_execution_enabled`。
逐 symbol 的下单模板跟随同一映射，被过滤的目标不会留下悬空的执行参数。
账户整体倍率继续由 shares / Virtual 跟随倍率决定，交易所杠杆不参与数量换算。

## 发布前预览与归档

账户「策略启用」页每个绑定都有「预览发布目标」。页面列出输入与执行 symbol、
单位系数、最终 qty / signal、过滤数量和原因。查询只读，不发布、归档或调用账户 API。
预览使用已保存的 shares；编辑后先保存再重新预览。真正重推时重新检查当前规则。

API：

```text
GET /api/catalog/accounts/{source_id}/bindings/{binding_name}/target-preview
```

需要登录、账户 view 权限及策略可见权限；返回 `source_id`、`binding_name`、`shares`、
`proxy_enabled`、`targets`、`symbol_order_strategy_overrides` 与 `projection`。
`targets` 是经 signal 执行开关处理后的运行时目标；启用 proxy 时 `projection`
包含规则抓取时间、匹配结果、乘数比、过滤原因及保留原 signal 的换算目标。
未启用时 `projection=null`。

PostgreSQL 保存原始策略目标。Manager 自有 RocksDB 的同一条 position update
消息在账户条目上追加 `target_projection`，冻结当次实际使用的换算目标、shares
结果、乘数比、过滤结果和规则时间。自动推送、手动 proxy 重推、停止及 Virtual
配置发布都先归档再发布同一份结果。失败重试可能产生新的尝试记录。
重建目标历史优先使用归档的账户目标，不重复乘 shares，也不拿今天的映射重算旧消息。
旧消息缺少此字段时沿用其原来的数量规则。无 PostgreSQL schema 变更或启动 DDL。
本功能不为 Bybit 启用 Binance Kline / funding 理论估值，不改 factual fill FIFO。

## RX02 只读核对

2026-10-19 10:17 UTC，核对 JP `binance_exec_trade05` 的十个策略绑定，
以及 SG `bybit_exec_trade01` 的完整 Bybit 规则缓存（798 个合约）：

| 范围 | 独立 symbol 数 | Bybit Trading 同名匹配 | 不匹配 | 单位系数 |
| --- | ---: | ---: | ---: | ---: |
| 唯一启用策略 `lgq_cta_symbol_mean` | 47 | 47 | 0 | 全部 1 |
| 十个绑定的目标并集 | 51 | 51 | 0 | 全部 1 |

51 个 symbol 为：1INCH、AAVE、ADA、ALGO、ATOM、AVAX、AXS、BAND、BEL、
BNB、BTC、COMP、CRV、DASH、DOGE、DOT、EGLD、ENJ、ETC、ETH、FIL、GRT、
IOST、IOTA、KAVA、KNC、KSM、LINK、LTC、NEAR、NEO、ONT、QTUM、RLC、RSR、
SKL、SNX、SOL、SUSHI、THETA、TRB、UNI、VET、XLM、XRP、XTZ、YFI、ZEC、
ZEN、ZIL、ZRX，均带 `USDT` 后缀。覆盖只代表该次快照，未来目标仍按缓存过滤。

操作员已确认目标是当前页面的 bybit-01；它的 source 为
`bybit_exec_trade01`，实际使用原 arb02 账户。没有更换成原 arb01 的凭据。
核对结果保存在 `artifacts/bybit_target_proxy_20261019/coverage.json`。

规则字段及分页依据：[Bybit instruments API](https://bybit-exchange.github.io/docs/v5/market/instrument)。

## 本地验证

`cargo fmt --check`、`cargo check`、`cargo test` 和 `cargo build --release` 通过。
全量测试为 367 项 library 测试加 9 项 snapshot binary 测试；原有 16 项
需要外部环境或手动执行的测试保持 ignored，其中 Redis 项另行通过隔离服务器执行。
该 Redis 测试覆盖缓存未加载、账户隔离、失败刷新保留缓存、合约数量换算、
逐 symbol 参数及 signal、clear 和全过滤后的完整向量替换。
归档测试覆盖原策略不变、账户间隔离、换算目标不重复乘 shares 和归档重放。

前端 `npm run build`、23 项既有测试通过；lint 没有新增告警。
首次本地验证记录在 `artifacts/bybit_target_proxy_20261019/`；该阶段没有线上操作。
后续 SG 部署及 RX02 双目的地投递的核验记录在
`artifacts/sg_target_proxy_20261019/`。
