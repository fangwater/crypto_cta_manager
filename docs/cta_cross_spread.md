# OKX / Binance Spread account

2026-11-04: the public alias is `okex_binace_cross`, with the fixed-frequency
suffix removed. Daily profit aggregates legs from the same Shanghai trading day;
independent exchange update timestamps need not match to the millisecond. The
displayed data time is the oldest leg observation. A missing profit or a leg from
a different Shanghai day still leaves the total unknown. This changes only the
account projection, preserving each leg's daily equity anchors and NAV inputs.
Audit: `artifacts/cross_account_name_profit_20261104/`.

Funding on the execution page retries the same account and time window every
ten seconds while public rates load, and stops on collapse or a range change.
An unknown initial amount displays a loading state. Known partial values retain
their coverage notice. OKX cold loads request the queried retained interval,
then extend it if an older interval is selected; previously loaded settlement
marks are reused. Binance prewarming, rate limits, source isolation and the
independent factual-income collection settings are unchanged.

JP enables cumulative daily Funding estimates. A public-symbol cache entry can
therefore serve this new account's launch-day query and an older account's
longer history. Older work must not define the launch-day query's loading state.
Cold cumulative loads publish recent requested coverage first; earlier coverage
extends afterward. Fetch slots are released between HTTP pages. A query whose
own interval and settlement marks are complete reports ready even while earlier
history is still loading. This account's estimate counts only settlements inside
its requested interval, using its own reconstructed pre-settlement holdings.

Managed spread queries also prioritize missing rates/marks for their selected
window within the same shared request budget and WAF backoff. Priority ends once
that window is covered, or expires after the requester stops refreshing. Other
accounts' older symbols cannot consume the whole queue first. The cache remains
shared and in flight loads are reused. No pre-launch Funding is added to this
account. Audit: `artifacts/fr_overview_and_funding_20261103/`.

Live equity subscribes to both enabled managed-account members even when their
standalone `monitor_enabled` switches remain false. Those switches still control
the independent health monitor. Dashboard cards, exchange equity and the account
overview use the two account-monitor totals; combined equity requires both legs.
Risk ratios remain venue-specific. This reuses account IPC and does not introduce
another exchange equity poll or activate standalone alerts.

2026-11-03: this subscription fix was published to JP Manager only, with the
existing frontend preserved. Both legs reported fresh equity; the public card's
54,497.17 USDT equaled OKX 34,808.64 plus Binance 19,688.53 at verification.
The account overview also recovered. All 117 protected processes and the
configuration, bindings, templates and immutable snapshots were preserved;
lgq visibility and prc exclusion passed. Audit:
`artifacts/cross_account_equity_20261103/`.

2026-11-03: JP Manager now retains confirmed OKX history candles when another
row is still `confirm=0`. That minute stays unpriced and is retried through the
existing NAV loading response. Fixed account/strategy NAV was identical across
the API-only deployment, and current-range valuation succeeded. Audit:
`artifacts/okx_candle_confirmation_20261103/`.

Current UI contract, confirmed by the operator on 2026-11-03: **one public
Manager account, with two independent Execs inside it**. Display alias:
`okex_binace_cross`. Internal account ID: `cta_cross_spread_rx01`.
The earlier deployment that exposed two ordinary accounts was a misunderstanding;
its receipts describe historical state only. Frontend release: `cross01_unified_viz_20261103T054937Z`.

| Execution alias inside the account | Stable source / environment | Market | Viz / Config | Authenticated gateway |
| --- | --- | --- | --- | --- |
| `okex_exec_cross01` | `okex_exec_trade01` | OKX Futures | 10049 / 18169 | `/exec_okex01/` |
| `binance_exec_cross01` | `binance_exec_spread01` | Binance USD-M | 10050 / 18170 | `/exec_binance_spread01/` |

The two Exec names identify execution sources. They must not become separate
account-list, NAV or strategy-selection entries. Manager exposes the saved
public alias once across those views. `cta_managed_account_aliases` stores the
public display alias; native execution aliases retain their existing catalog
fields. The public account does not create a new source row or order store.
The host configuration and deployment fragment retain this mapping:

```toml
[accounts.cta_cross_spread_rx01]
name = "cross01"
source_ids = ["okex_exec_trade01", "binance_exec_spread01"]
created_at_us = 1793625556000000
```

`created_at_us` retains the original business-account inception, not a new
position or equity snapshot. The saved alias overrides the fallback name.
Source IDs, environment directories, namespaces and historical records remain
unchanged. OKX credentials come from the stopped `okex_mm_alpha`; Binance uses
81857 in STANDARD USD-M mode. Credentials stay only in remote environment files.

The ordinary account page is
`/manager/account/?source=cta_cross_spread_rx01`. Its market selector changes
only the execution settings being displayed. Fee, leverage and risk requests
remain scoped to the selected native source; never fan out an ambiguous write.
Both account and strategy pages keep the public account scope in their links.
Same-name strategy rows appear once, retaining each market's algorithm and
shares. Independent source/symbol/venue FIFO is calculated before addition.
The account NAV and same-name strategy PnL are the sum of the two sides.

The account card and account page link to
`http://13.115.227.29:4191/manager/exec/?group=cross01`. This is the account's
execution page: one account summary, one execution table and shared exchange,
strategy and symbol filters. It directly consumes the two authenticated native
WebSocket streams and snapshot endpoints; there are no embedded Viz pages or
iframes. Rows retain source, market, symbol and strategy identity. Symbol
groups keep the markets separate; same-name strategy groups combine their
monetary execution totals and expand to venue-specific details. Account
configuration, NAV and strategy links all return to the same public account.
Metadata polling and manual refresh preserve the existing stream connections.

Equity and notionals add across fresh sources; combined leverage is total gross
notional divided by total equity. Pending and live order amounts use absolute
values so opposing execution work does not cancel. Repeated account positions
are counted once per source/market/symbol; strategy filters use strategy-owned
positions. Quantities from different markets or symbols are not added.
Missing, disconnected or older-than-five-second producer data makes the
affected totals unavailable instead of zero. Older snapshots cannot overwrite
newer live events. Source authorization failures clear that source's state.

The expandable combined strategy-PnL section queries the existing timeline API
using the public account ID. The API expands it to the exact two native sources;
the frontend validates the returned source scope. Fee-after trading PnL and
public estimated funding are added after isolated accounting. 1D/7D/30D/ALL
ranges and explicit refresh are available. Missing funding remains unavailable.

The execution-page resolver requires the public account and its complete
`execution_sources`; a flat list of two native accounts cannot stand in for it.
Both members must be enabled and fully visible. Host `dashboard.source_viewers`
keeps the account and both native gateways lgq-only, including against admin
prc. Incomplete account access reveals neither execution rows nor member names.

Both factual funding collectors remain disabled with zero saved income
records. Actual-collection controls are hidden for this account. Public OKX
settlement rates, pre-settlement closed mark minutes and 1min trade candles
are already supported; official BTC samples validated 10 consecutive candles,
three funding settlements and a settlement mark candle. See mkt_signal
`artifacts/cta_spread_data_audit_20261103/`. No native OKX private funding
collector is needed.

The strategy accounting is fresh: no historical trade08 orders, initial
account/strategy positions, allocation snapshots or execution bindings. Trading
and signal publication remain off. Each Exec retains independent risk,
persistence and source IPs. The previously observed physical Binance IOTAUSDT
-0.1 residual was not closed or seeded into the strategy ledger; an empty
strategy ledger does not establish physical exchange flatness.

The unified execution release changes only Manager frontend. The public-account
mapping, API and runtime configuration remain unchanged. It reuses API SHA-256
`aff36d8b35544952307f3ad3348122f979336c50a7c438d36268427298acc100`.
Manager PID3538262 remains on CPU24; no process restarts. All 87 protected
process identities, source configs, snapshots and bindings are retained.
No Nginx, trading, source-IP or routing changes. Frontend tests (52) and the
production build pass. Chromium verifies one public account, one summary,
one execution table, zero iframes, two native WebSocket upgrades (101), stable
connections through refresh, mobile layout and denied-user access.
Browser-only fixture events verify aggregation, shared filters, live updates,
stale data, out-of-order snapshots and isolated access failures; actual native
execution snapshots remain empty because execution is off. Deployment and
browser receipts are in mkt_signal `artifacts/cross01_unified_viz_20261103/`.
Private rollback/audit files are in
`~/crypto_cta_manager/maintenance-backups/cross01_unified_viz_20261103T054937Z/`.

All hosts use the standard release build. Candidates must pass
`cta_web --config <candidate> --check-allocations` with the actual database.
Build locally and verify uploaded artifact hashes before installation.

Manager supports one position strategy containing venue-qualified paired USDT
perpetual targets. For example `okex-futures:ETHUSDT` +2 and
`binance-futures:ETHUSDT` -2. Bind that position strategy to an existing OKX
unified Exec and an independent Binance USD-M Exec with matching shares.
Each binding receives only its venue's ordinary `ETHUSDT` target. The OKX account
stays unified; no account-mode mutation is introduced. A directional entry may
have a nonzero OKX target and an explicit zero Binance target. Removing a symbol
requires explicit paired clear targets before omitting it, as with other strategies.

Both market keys are required per asset, including zero and clear targets.
Do not mix these keys with ordinary targets in one strategy. Other exchanges and
Bybit target proxies cannot bind this mode. Catalog JSON uses the existing tables;
no database maintenance or new internal API format is required. Account projections
and effective shares are frozen in the existing position archive before publish.
Each Exec keeps its existing market ownership and receives normal unqualified symbols.
Execution and delivery across the two accounts are independent and retryable, not atomic.

The existing target editor and publish client accept these target names. Parameter
overrides use the same qualified keys and are projected to the matching account.
Native OKX still owns its separate cash market, but these targets only address
perpetuals. Account NAV selection and strategy permissions keep their existing scope.
