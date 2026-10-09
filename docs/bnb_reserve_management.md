# BNB reserve management

BNB management is configured per `source_id` on **自动理财与 BNB 管理**.
It is disabled by default and starts in dry-run mode. This implementation targets
Binance STANDARD USD-M accounts. It does not deploy or enable any live account.

## Operator settings

The two refill quantities are independent, editable settings, not hard-coded
VIP offsets:

| Setting | Default | Meaning |
| --- | ---: | --- |
| `required_bnb` | 5 | Operator-selected VIP BNB requirement |
| `refill_trigger_bnb` | 5.2 | Start buying when managed total is at or below this |
| `refill_target_bnb` | 6 | Complete a triggered refill to this quantity |
| `futures_trigger_bnb` | 1.2 | Replenish the futures fee wallet |
| `futures_target_bnb` | 1.5 | Fee wallet refill target |
| `futures_sweep_bnb` | 1.8 | Sweep excess futures BNB toward Flexible Earn |
| `earn_min_bnb` | 0.1 | Minimum surplus subscription |
| `hedge_tolerance_bnb` | 0.5 | Minimum factual net exposure for a new hedge target; configurable at or above 0.5 |
| `hedge_min_interval_secs` | 3600 | Minimum interval between hedge target updates; configurable from 1 to 24 hours |
| `interval_secs` | 60 | Normal balance check interval |
| `max_conversion_usdt` | 10000 | Per-conversion cost cap |
| `max_quote_deviation_bps` | 100 | Maximum quote cost above the spot BNB reference |

For a 25 BNB requirement, configure 25 / 25.2 / 26, or another explicitly
chosen trigger and target. Validation requires `required < trigger < target`.
The target is never reduced automatically based on the exchange's current VIP
level. A refill already in progress remains latched until confirmed complete.
A small fee charged just after that purchase does not initiate another refill.
This maintains the BNB leg of VIP eligibility; trading-volume and other VIP
qualification rules remain exchange-owned.

## Funds and exchange routes

The managed reserve is real BNB in the account's Spot wallet (free + locked),
USD-M wallet, and all Simple Earn Flexible BNB positions. Earn receipts such as
LDBNB are not added again. Funding, Margin, locked Earn products and other
subaccounts are not managed by this module; do not treat this quantity as a
master-account VIP aggregation. No futures position quantity is counted as BNB
ownership. A missing/invalid balance response blocks actions instead of becoming
zero. Earn reads are paginated.

At the refill threshold, use only **BFUSD → BNB direct Convert**. Discover
support first in the futures wallet, then in Spot; transfer available BFUSD
between those wallets only when the selected direct route requires it. An
unsupported pair is reported and the refill remains pending for a later check.
A network/auth/parse failure is not interpreted as an unsupported pair. Pair
discovery is cached for 24 hours. Quotes request the BNB amount needed and
validate amount, expiry, cost cap and price deviation before acceptance.

This controller neither redeems BFUSD nor spends/transfers USDT. USDT is used
only as the price-reference and cost-limit unit. Transferability and margin
headroom are required even when the BFUSD wallet balance is positive. The module
retains a margin buffer of three times maintenance margin. The intended live
account must expose a BFUSD/BNB direct pair and the required API entitlements;
offline fixtures establish behavior, not live pair availability.

The futures fee wallet uses its own thresholds and consumes existing Spot/Earn
BNB first. Earn redemptions are checked against the VIP requirement because BNB
in redemption can be temporarily excluded from Binance's daily-average count.
If a redemption would cross that requirement, refill the total reserve first.
The BNB fee-discount switch is enabled for the USD-M account. Excess transferable
BNB goes to Spot, then to available Flexible Earn products within personal quota.
Manager subscriptions use `autoSubscribe=false` so the exchange auto-sweep does
not compete with the fee-reserve workflow.

## Execution ownership and recovery

### 已确认的 BNBUSDC 专用对冲方案

BNB 储备对冲选择 **BNBUSDC 永续合约**，由受保护的
`SYSTEM_BNB_RESERVE` 系统策略独占管理。普通 CTA 策略不得在采用此方案的
账户上交易 BNBUSDC；CTA 的 BNBUSDT 持仓与储备对冲按不同合约区分。
普通策略目标发布和账户绑定校验执行专用限制，允许零目标用于清理已有仓位。
该限制按已配置 BNB 管理的账户生效，包括关闭自动管理但保留对冲的账户；
其他未采用此方案的账户仍可以交易 BNBUSDC。

**分开的是合约持仓和策略归属，不是保证金风险。** BNBUSDC 与 BNBUSDT
都属于 USD-M。trade03 当前开启多资产模式，两者共享保证金，盈亏也会影响
同一账户的可用保证金；BNBUSDC 的盈亏和资金费以 USDC 结算。此方案沿用
现有 USD-M Exec，不需要币本位钱包或币本位 Exec，但不能描述为独立资金账户。
参见 [Binance 多资产模式说明](https://www.binance.com/en/support/faq/detail/29b45c485d664028b9ca1cdf90b24f6f)。

BNBUSDC 是线性合约，目标空头数量按实际持有的 BNB 数量计算，并遵守
该合约自身的数量步长、最小数量及最小名义金额。BFUSD → BNB 直接闪兑、
VIP 门槛与补足阈值、USD-M 手续费备用金及 Flexible Earn 分配继续按本说明
运行。原 USDT/BFUSD 理财的配置和调度保持独立；这种流程独立不代表保证金隔离。

**实施状态：已与虚拟账户联合发布到 jp-meta trade03。** 当前 Manager
Manager 常态对冲精简版 release 为 `20261009T055537Z`（`af85845`），
配套 Exec/公共行情继续使用 `8a263e04`。
BNBUSDC 的报价、杠杆初始化和实际成交已验证。
切换时须先检查
该账户已有的 BNBUSDC CTA 目标、挂单和事实持仓。trade03 已完成旧 BNBUSDT
系统空头退出与新 BNBUSDC 空头建立，自动迁移代码已删除；原 CTA 的 BNBUSDT
目标与持仓归属仍独立。其他账户若仍有非零旧系统目标，必须先显式完成切换，
常态对冲不会自动接管或迁移这些仓位。

### 对冲执行与频率控制

Manager publishes a protected system strategy named `SYSTEM_BNB_RESERVE` with
one BNBUSDC target equal to minus the managed BNB quantity. It uses the existing
source/venue-scoped Redis writer, readback and Iceoryx reload notification.
Exec owns all futures order submission, order rules, reconciliation and fill
persistence. No independent exchange futures-order client is introduced.
Normal catalog publish, edit and removal validation rejects this reserved name.
The existing Exec ledger isolates it from CTA strategies trading BNB; ordinary
CTA target changes never zero this hedge. Disabling BNB automation freezes its
last hedge target rather than silently closing the hedge.

Balance checks default to once per 60 seconds. A normal hedge update requires
both factual net exposure of at least 0.5 BNB and at least 3600 seconds since the last
target publication (at most one update per hour). A price change alone
does not change the quantity target of this linear hedge. The configurable
minimum interval is 3600–86400 seconds (shown as hours in the UI); manual rounds and restarts do not bypass
the persisted interval or the Redis target timestamp. An incomplete prior target,
pending order, missing allocation or stale Viz snapshot blocks further updates.
Reserve replenishment still runs before the hedge and is not delayed by its
cooldown. HTTP weight limits and exchange cooldowns apply independently.

There is one steady-state USDC hedge path, with no automatic legacy migration
or separate short-interval exception. Existing nonzero BNBUSDT reserve targets
block updates until an explicit switch is completed. The adjustment threshold
is independent of the VIP refill cushion and of execution precision: when an
update triggers, the USDC target is the full negative reserve quantity, not
increments of 0.5 BNB. Previous targets must complete within 0.02 BNB, without
live orders; account/strategy ownership uses the same strict settlement bound.
Exec may retain a small unfilled residual as `pending_qty` after completion;
this must remain within tolerance. A fresh, position-ready snapshot may omit
a fully idle zero position; missing nonzero targets still block progress.
Completed zero-target `SYSTEM_POSITION_CLOSE` rows may contain f32 account-IPC
rounding differences against f64 fill allocations. Only these idle rows may
ignore quantities within two relative f32 epsilon units, capped at 0.00001 BNB;
other strategies and active orders retain strict ownership checks.
An existing explicit zero BNBUSDT target remains to prevent its resurrection.
Status reports any factual legacy fill tail and includes it in net exposure.
Existing other-strategy BNBUSDC targets, holdings or orders, and unexplained
account holdings defer adjustment instead of taking ownership of those positions.

Reserve execution uses one order per batch, a 500 quote-unit baseline order size,
at most two batches, a five-second batch interval, a ten-second maker timeout and
at most one maker requote. Exec may raise the order size for a larger target to
stay within the batch count. These limit traffic within each target execution; the normal
Exec-wide order rate controls continue to apply. Target publication counts are
not an exchange order-count limit and are not a guarantee against shared-IP usage.

Hedge updates require a recent, position-ready Exec Viz snapshot. A hedge error
does not block reserve purchases. The status includes the actual strategy
quantity reported by Viz and any hedge error. Futures fills retain their system
strategy attribution and show as **BNB 储备对冲**. Current CTA NAV does not include
the corresponding Spot holdings, Earn rewards or funding, and is not a complete
performance report for this reserve portfolio.

BNB management has its own per-account enable switch, settings, timer and
operation journal. It does not service BNB before USDT/BFUSD subscriptions, read
BFUSD's pause state or change the original controller's thresholds or schedule.
A BNB refill failure does not pause USDT/BFUSD automation, and a disabled or
paused USDT/BFUSD controller does not disable BNB management. The controllers
share only a funds-operation mutex to avoid simultaneous wallet mutations and
HTTP rate budgets for the same egress. Run a single `cta_web` owner; these are
in-process protections rather than cross-process distributed locks.

`config/bnb-auto.json`, beside `config/bfusd-auto.json` under the Manager root,
is written atomically with file and directory fsync and mode 0600. It contains
settings, the latched funding target, a pending action, and the last 100 confirmed
operations/operator acknowledgments. Each financial POST is journaled before
submission. Pending actions survive restarts and block subsequent mutations.
Convert results are queried by the persisted quote ID; successful acceptance
alone is not considered final. The receiving balance must also be observed.
An unknown non-Convert response requires operator reconciliation, not blind
resubmission. The UI acknowledgment requires a checkbox and the exact pending
operation timestamp to prevent acknowledging a different operation by mistake.

## Configuration and API

Configure one explicit egress policy for both asset controllers. A dedicated
address remains available, for example on el01:

```toml
[treasury]
local_ip = "154.197.32.9"
```

By operator choice, jp-meta instead reuses each account's existing addresses:

```toml
[treasury]
use_account_ip_rotation = true
```

The two settings are mutually exclusive. Rotation reads only the source's
`trade_engine.toml local_ips`; it does not modify that file or add any address.
BNB and BFUSD share a per-source round cursor. Each complete round uses one
explicit bound address, then the next round advances to the other address.
No ambiguous financial submission is retried on a different IP. Unspecified,
loopback and empty address lists are rejected. There is no default-route fallback,
and clients disable HTTP environment proxies. With fixed dedicated egress, the
existing trading-IP collision checks and el01 reserved-address checks still apply.
On jp-meta the selected rotation is `.228` / `.234`, while the separate Kline
client retains `.93` and its own public-egress exclusions and 600/min budget.

All BFUSD and BNB HTTP clients share process-wide rate-limit state by bound
local IP and API origin, across accounts and repeated client creation. Rolling
60-second reservations count unsuccessful requests too. Conservative budgets
are 1200/min for USD-M, 4500/min for Spot and SAPI IP operations, and 6000/min for
SAPI account operations (the latter also aggregated across accounts on the IP).
The weight table includes high-cost Convert discovery (3000), transfers (900)
and quote acceptance. USD-M quotes also respect 360/hour and 500/day rolling
limits, and Simple Earn writes have a three-second cooldown. Quote requests
require headroom for acceptance before requesting the ten-second quote.

Response weight headers account for exchange-observed use by other clients.
HTTP 429/418 applies the `Retry-After` cooldown to every asset-management pool
on that IP/origin; a missing header uses a conservative fallback. Exhaustion
returns before sending and is retried in a later round without sleeping on the
funds mutex. BNB reserves write weight before creating its pending journal;
a request that never left this process cannot become an ambiguous transaction.
USDT/BFUSD reserves its complete write plan before pausing for a transaction.
Other processes and egress IPs sharing a public NAT are not coordinated by this
in-process limiter. Its counters reset on process restart; exchange headers and
cooldowns remain authoritative when the next response arrives.

The deployment preflight can also run without the web server or browser token:

```bash
cta_web --config config/cta-manager.toml --bnb-preview-source binance_exec_trade03
```

This reads the saved BNB settings and exchange balances and prints the proposed
reserve/hedge quantities. It does not start the Manager server, initialize a
database, open Manager/Exec RocksDB, accept a quote, change the financial journal
or publish a hedge. It is suitable before activating a replacement binary.

Account-scoped endpoints are under `/api/catalog/accounts/{source_id}/bnb-auto`:

- `GET`: saved settings and the last observed status; does not contact Binance.
- `PUT`: save full settings.
- `POST /preview`: read balances and report a plan; no exchange mutations,
  quote acceptance, settings journal changes, Redis writes or hedge orders.
- `POST /run`: execute one round using the saved enabled/dry-run settings.
- `POST /acknowledge`: `{"pending_at_ms": ..., "exchange_outcome_verified": true}`.

The existing account visibility/configure authorization applies, and mutation /
preview endpoints additionally require the existing `X-BFUSD-Operation-Token`.
Tokens and exchange credentials are not returned to the browser or written to
BNB state. Preview is available while automation is disabled. Enable actual
execution only after checking the intended source, thresholds, API permissions,
dedicated egress and reserve ownership. This does not require a schema migration
or writing to any Exec RocksDB.

Official contracts consulted:

- [Spot Convert](https://developers.binance.com/en/docs/catalog/core-trading-convert/api/rest-api/trade)
- [USD-M Convert](https://developers.binance.com/en/docs/catalog/core-trading-derivatives-trading-usd-s-m-futures/api/rest-api/convert)
- [BFUSD](https://developers.binance.com/en/docs/catalog/investment-and-services-simple-earn/api/rest-api/bfusd)
- [Simple Earn Flexible](https://developers.binance.com/en/docs/catalog/investment-and-services-simple-earn/api/rest-api/flexible-locked)
- [BNB balance calculation](https://www.binance.com/en-NG/support/faq/detail/a4423d2a5252446388208ef7c4ae3039)
