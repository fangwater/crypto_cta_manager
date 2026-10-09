# jp-meta trade03 BNB management deployment

## 2026-10-09 联合发布：BNBUSDC 与虚拟账户

当前 Manager release 为 `20261009T053323Z`，运行代码 `1b285b5`。
虚拟账户已随本次发布启用；jp-meta 的四张新增表和索引在完整 PostgreSQL
备份后显式事务创建，没有启动 DDL、数据重置或真实账户跟随切换。
空模板的创建、读取、删除已验证。首次联合 release `20261009T043909Z`
运行 `5a05f28`，随后补充对冲完成尾差和空闲零仓位快照的处理。

trade03 储备使用 `SYSTEM_BNB_RESERVE` 独占的 **BNBUSDC 永续**。
普通 CTA（包括虚拟账户跟随）禁止对该账户发布非零 BNBUSDC 目标。
BNBUSDT 的 CTA 目标和分配独立保留。分开的是合约和策略归属；两个 USD-M
合约在该账户的多资产模式下仍共享保证金，BNBUSDC 盈亏和资金费以 USDC 结算。

本次配套发布 Exec `8a263e04` 的 `exec-pre-trade`、`account_monitor`、
`viz_server`，保留独立的交易引擎、持久化和 Config 服务。
修复杠杆初始化误将 `BNBUSDC` 变成 `BNBUSDCUSDT` 的问题；初始化失败按
每合约至少 60 秒重试，避免 Redis 高频 reload 反复请求交易所。
账户监控和 Viz 同步更新当前风控 IPC、紧凑仓位快照及 64 KiB 通道。
Viz TOML 仅新增明确的 `venue = "binance-futures"`。

原公共行情二进制没有 USDC 覆盖，本次同时更新共享的 Binance Futures
`spread_pbs`（同一代码 `8a263e04`），通过环境启动脚本恢复 BBO/market
两个进程。启动日志确认 779 个活跃 USDT/USDC 合约，BNBUSDC 报价和实际成交
已验证。原 CPU 9/14、行情配置和 IP 文件哈希不变；未增加地址或改变交易绑定。

余额检查默认 60 秒；稳态对冲至少间隔 300 秒且偏差超过 0.02 BNB 才调整。
迁移每轮最多转移旧储备仓 0.5 BNB，至少间隔 60 秒，并等待两边完成且没有
活动订单；已完成的数量尾差须在 0.02 BNB 容差内。仅空闲的零仓位可以从新
Exec 快照省略。账户 IPC 的 f32 数量与成交分配的 f64 数量可能产生系统平仓
尾差；只豁免已完成、零目标、无活动订单的 `SYSTEM_POSITION_CLOSE` 行，
限额为两倍相对 f32 epsilon 且最多 0.00001 BNB。真实外来策略持仓仍阻止迁移。
默认补仓阈值/目标继续为 5.2 → 6，直接用 BFUSD 闪兑 BNB。
原 USDT/BFUSD 配置文件字节一致，trade04 的 BNB 管理保持关闭。

备份和验证证据位于：

`/home/ubuntu/crypto_cta_manager/backups/combined-20261009T043632Z`

保留完整数据库 dump、旧 Manager/Exec/公共行情二进制、原 Viz 配置、
发布前进程与配置哈希、联合发布和逐步迁移观测。凭据不复制到说明或清单。
Manager 二进制 SHA-256：

`2ae1555ab3cbec52488edae68227e134b1390ca217aa1bdfe501eb348b055103`

Exec pre-trade SHA-256：

`2455759e096dfefaf67a8cd7c46de94f881817768187b84261c9062a2771b13f`

本次代码验证：此前全 crate 测试 277 通过、8 个外部环境测试忽略；
BNB 管理 16 项、Exec 杠杆/重试 20 项、风控 IPC 10 项及 Viz 编解码 2 项通过，
零仓位省略追加测试通过。Rust 格式检查、本地 release 和前端 build 通过。
完整方案见 [BNB 管理说明](bnb_reserve_management.md#已确认的-bnbusdc-专用对冲方案)。

## 联合发布最终验证

2026-10-09 05:39:20 UTC，trade03 持有 5.77770585 BNB，实际 BNBUSDC 储备
空头为 -5.770000257492065 BNB，旧 BNBUSDT 储备尾差为 -0.005630475613392472
BNB。两者合计后的净敞口为 +0.0020751168945432052 BNB，低于 0.02 BNB 容差。
旧储备目标明确为零，两个目标均已完成且没有活动订单。仅退出储备系统份额，
原 BNBUSDT CTA 分配继续独立运行。最末目标发布时间为 05:36:39 UTC，
后续余额检查没有重复发布。

迁移恢复后的 10 次发布中，最短间隔为 60,140 毫秒，最大旧仓转移量为
0.5 BNB。稳态设置为至少 300 秒且偏差超过 0.02 BNB；重启和手动轮询沿用
保存的发布时刻。没有 BNB 兑换或转账待核对，当前余额未触发 5.2 的补仓线。

Manager 健康为 `ok`，BNB 对冲错误为空。虚拟账户空模板 CRUD 通过，
真实跟随数和待发布队列均为零；所有真实账户仍使用原配置。
Manager/虚拟账户/理财页面及 trade03 Viz、Config、snapshot 网关均返回 200，
Exec WebSocket 返回 101。临时验证模板及管理员会话已删除。

原始进程基线含 98 项，其中 2 项是非交易辅助进程。有意更新 trade03 的
pre-trade、账户监控、Viz 和两个共享 Binance Futures 行情进程；其余 91 项
运行标识不变，没有意外变化。全部 26 项交易/环境配置哈希不变，Manager TOML
和 USDT/BFUSD 配置文件字节一致。Viz 只新增市场声明；公共行情的 IP、配置和
CPU 9/14 保持原样。交易引擎、持久化、Config、Nginx 及其他账户没有重启。

`FINAL-RESULT.json`、`VERIFICATION.json`、`MIGRATION-COMPLETE.json`、
`MIGRATION-OBSERVATIONS.jsonl`、`FREQUENCY-VERIFICATION.json`、
`EXEC-REPAIR.json` 和 `PUBLIC-FEED-REPAIR.json` 记录上述观测及二进制校验。
备份目录保留完整回滚材料；仅删除临时验证辅助程序和会话。

## 首次 BNBUSDT 发布（20261009T033154Z）

以下记录仅描述首次 BNBUSDT 版本。该版本通过已有 BatchExec 合约运行，
当时只替换 Manager 和前端，没有更新 Exec。原独立 CTA BNB 策略为
`rbf_big` 和 `rbf_mid_1min`。原 USDT/BFUSD 自动理财在 trade03/trade04 均开启，
间隔 3600 秒、每轮上限 5000 USDT、触发 100 USDT，配置和操作令牌须保留。

## Prepared configuration

| Setting | Value |
| --- | ---: |
| Required BNB | 5 |
| Refill trigger → target | 5.2 → 6 |
| Futures fee-wallet trigger → target | 1.2 → 1.5 |
| Futures sweep threshold | 1.8 |
| Flexible Earn minimum | 0.1 |
| Hedge quantity tolerance | 0.02 |
| Next-release normal hedge minimum interval | 300 seconds |
| Normal check interval | 60 seconds |
| Conversion cost cap | 10000 USDT equivalent |
| Quote deviation limit | 100 bps |

Only trade03 receives a BNB account entry. The prepared entry has `enabled=true`
and `dry_run=true` for initial verification. Actual execution can be enabled
after confirming balances, direct-route funding and permissions. The existing
Manager TOML is copied into staging with only `[treasury].use_account_ip_rotation=true` added;
sources, Kline configuration and dashboard settings are retained exactly.

Staging directory on jp-meta:

`/home/ubuntu/crypto_cta_manager/deploy-staging/bnb-trade03-20261009T031429Z`

Prepared files include the Manager binary, frontend, proposed TOML/BNB settings,
artifact hashes and a baseline of protected process IDs and live configuration
hashes. The release was activated as `20261009T033154Z`; temporary staging was removed
after verification. The retained release contains the source patch, hashes,
verification evidence and rollback artifacts.

## Egress and API findings

The operator explicitly chose existing account IP rotation:

| Bound local IP | Public egress |
| --- | --- |
| `172.31.35.228` | `13.115.227.29` |
| `172.31.35.234` | `54.64.228.233` |

Both trade03 and trade04 private Spot/USD-M account reads succeeded on both
addresses. Trade03's API allows reading, Spot trading, futures and universal
transfers. No IP binding, account allowlist or trade-engine configuration is
changed. BNB and BFUSD share the per-source rotation cursor, IP/origin request
weight budgets and funds mutex. Each round keeps the chosen address for all
requests, rather than rotating in the middle of a financial transaction.

The previously proposed `.93` / `18.181.48.65` treasury binding was rejected by
private APIs and is not activated. The separate Manager Kline client continues
using that address with its existing 600/minute budget and exclusion list.

Read-only pair discovery confirmed that Spot provides BFUSD → BNB, while USD-M
has no direct pair. Refills therefore use direct Spot Convert, transferring
BFUSD from the futures wallet when required. No BFUSD redemption or USDT
purchase fallback is permitted.

Before deployment, trade03 held 1.30897639 BNB in USD-M and 4.48102103 BNB in
Flexible Earn, approximately 5.79 total. No refill is due at the 5.2 trigger.
The initial independent hedge target should therefore be approximately -5.79
BNB. Existing CTA BNB allocations remain independent. Credentials and operation
token values are not printed or copied into this document or artifact manifests.

## Activation and verification

Verify uploaded hashes, back up the
exact live Manager binary/TOML and frontend link, atomically install the prepared
configuration and binary, and switch the frontend. Restart only
`crypto-cta-manager-web.service`. Verify the dry-run plan and actual BNB wallet
balances before enabling live BNB operations on trade03. Keep trade04's BNB
management disabled. Check that original USDT/BFUSD settings remain unchanged.

Verify the protected strategy's target against total Spot + USD-M + Flexible
Earn BNB, its factual position through Viz, and any pending operation journal.
Compare protected process IDs and trade-engine/environment hashes with the
baseline. Exec Config, Viz, WebSocket, Nginx and the other accounts remain live.
Record final release hashes and verification results here after activation.

Local validation passed: `cargo fmt --check`, `cargo check`, full `cargo test`
(272 passed across test binaries, 7 environment-dependent tests ignored),
`cargo build --release`, and `npm run build`.


## Activated release

Release `20261009T033154Z` is live. Manager binary SHA-256:

`b205afc5b2bd523e9a8b57f2c9254ba15125d1335f78f493538463cd267df898`

Only `crypto-cta-manager-web.service` restarted (PID 3439408 → 406245); all 67
protected trading, Exec, Viz, Config, Nginx and monitor processes retained their
PIDs and command lines. Trade03/trade04 environment and trade-engine hashes are
unchanged. The BFUSD settings file, including its operation token hash, is byte
identical to the pre-deployment file. Kline source, exclusions and 600/min budget
are unchanged.

Trade03 BNB management is enabled with `dry_run=false`, trigger 5.2 and target 6.
Trade04 BNB management remains disabled. A dry-run verified approximately
5.789 BNB and zero refill quantity before activation. The running reserve then
published `SYSTEM_BNB_RESERVE` to the existing Exec without an Exec restart.

A subsequent check observed 5.78698165 BNB in the reserve and a factual
-5.78867793 BNB reserved-strategy position, residual -0.00169628 BNB. Exec reported
`execution_complete=true`, `completion_reason=target_tolerance`; the residual is
within the configured 0.02 BNB tolerance. The Manager UI's `hedge_qty` reports
that factual strategy quantity. No refill purchase was due, no financial action
was pending and no hedge error was reported. `rbf_big` and `rbf_mid_1min` retained
independent BNB targets and allocations.

Both existing USDT/BFUSD controllers remain enabled and unpaused. Their first
rounds after restart skipped subscription because available USDT was below the
original trigger. Manager health returned `ok` with no refresh error. Authenticated
Manager, Exec Viz, snapshot and Config gateway checks returned 200; Exec WebSocket
returned 101. Artifact hashes matched the local release. The temporary admin
verification session was removed after the checks.

Evidence and rollback files are retained in:

`/home/ubuntu/crypto_cta_manager/web-releases/20261009T033154Z`

`VERIFICATION.json` records the balance/hedge observation and health checks.
`BASELINE.json` records protected process IDs and configuration hashes.
`rollback/` holds the previous Manager binary, TOML and frontend link target.
