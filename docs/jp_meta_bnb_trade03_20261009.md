# jp-meta trade03 BNB management deployment

## 后续确认：BNBUSDC 专用对冲

运营方已确认采用 BNBUSDC 永续作为 BNB 储备的专用对冲合约，与普通 CTA 的
BNBUSDT 持仓区分。普通 CTA 在该账户上应禁止使用 BNBUSDC。trade03 的
多资产模式下，两个合约仍共享 USD-M 保证金，不能视为资金风险隔离。

本文件记录的 release `20261009T033154Z` 仍使用 BNBUSDT；BNBUSDC 切换和
专用合约限制已在代码实现，按运营方最新指示等待虚拟账户功能一起发布，
本次未重启 Manager 或 Exec，未变更线上仓位。默认余额检查 60 秒；稳态对冲
至少间隔 300 秒且偏差超过 0.02 BNB 才调整。迁移时每分钟最多转移旧仓
0.5 BNB，上一轮两边都完成后再继续。完整约束与迁移要求见
[BNB 管理说明](bnb_reserve_management.md#已确认的-bnbusdc-专用对冲方案)。

Prepared on 2026-10-09 UTC for `binance_exec_trade03`. The operator selected
reuse of the two existing account IPs, replacing the proposed dedicated treasury
egress. Only Manager and its frontend need replacement; Exec remains compatible.

## Runtime compatibility

The live trade03 Exec exposes a fresh, position-ready `/snapshot`, including
per-strategy factual quantities for BNBUSDT. Its `exec-pre-trade` supports the
existing source/venue-scoped BatchExec Redis contract and independent strategy
ledgers. `SYSTEM_BNB_RESERVE` is absent from live Redis before activation.
This feature requires updating Manager and its frontend; it does not require
replacing or restarting trade03's Exec binaries.

The current independent CTA BNB strategies are `rbf_big` and `rbf_mid_1min`.
The reserve hedge must be published under its protected name without rewriting
those targets or their factual allocations. Existing USDT/BFUSD automation is
enabled on both trade03 and trade04: interval 3600 seconds, round cap 5000 USDT,
trigger 100 USDT, not paused. Preserve these settings and the operation token.

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
