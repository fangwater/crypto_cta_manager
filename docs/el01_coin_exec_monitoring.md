# el01 Exec market configuration and monitoring

Last updated: 2026-10-09 UTC.

Current state: zy_group26 (`binance_exec_trade10`) and zy_group29
(`binance_exec_trade11`) use Binance USD-M futures, following the operator's
correction on October 9. Trade10/11 have the latest Exec binaries and run only
account monitoring, Viz and Config. Bahll202210 (`binance_exec_trade01`) resumed
trading on the operator's subsequent instruction. The replacement trade05
account xy_lxy21 has a new source/namespace (`binance_exec_xy_lxy21_05`) and now
runs trading on the operator's later instruction, following c40 at multiplier
116. Matching initial account/strategy PnL anchors are now stored at the successful
startup time; both ordinary users' desktop/mobile strategy views are verified.
Manager includes the USDT reserve, Virtual
editor and owned/delegated Virtual permissions.
The latest verification is at the end; the COIN-M sections below are historical.

The operator identified two empty accounts as Binance COIN-M and authorized
republishing their programs and pages, explicitly requiring trading to remain
stopped while inspecting monitoring.

| Account | Stable source / namespace | Gateway | Viz / Config |
| --- | --- | --- | --- |
| zy_group26 | binance_exec_trade10 | /exec_trade10/ | 10050 / 18170 |
| zy_group29 | binance_exec_trade11 | /exec_trade11/ | 10052 / 18171 |

Both had incorrectly declared `binance-futures`. Their Exec persist directories
were empty, Manager had no position snapshots or strategy bindings for them,
and Viz snapshots contained no entries. Corrected `EXEC_VENUE` and `VENUE` in
each existing env, Config's venue, explicit Viz venue, and Manager's two source
venues to `binance-coin-futures`. IDs, namespaces, ports, credentials, trading
TOMLs, and existing archives were retained. No schema maintenance was performed.

Published all six Exec binaries and deployment scripts from synchronized
`mkt_signal` branch `arbmm`, commit `226f4e79`. The publish wrapper verified SHA-256
and stopped target processes before atomic replacements. Started only each
account's Config and Viz using its environment-local wrappers. No trading,
account-monitor, persistence, or signal process was started; no strategy target,
exchange order, cancellation, leverage, or account-mode change was submitted.

Manager runtime `910d67a` also binds public order-rule HTTP requests to the
configured dedicated market-data local IP and disables environment proxies.
el01 uses `154.197.32.10`; its public egress was verified during Kline enablement.
The Manager service was restarted once, PID 214461 after deployment. Existing
frontend `f989389` and webroot `web-releases/20261008T073040Z` were retained.
The root `RUNTIME-RELEASE.txt` records this backend replacement; the original
frontend release manifest describes its original deployment.

## Verification

- Manager formatting, check, full tests (255 passed, 7 ignored), and release
  build passed. Exec release builds and all six Viz tests passed.
- Authenticated gateway requests verified both coin pages, empty snapshots,
  coin Config bootstrap and independent Redis prefixes, and WebSocket 101.
  Manager dashboard reports both account labels and coin venues correctly.
- Manager's Redis rule snapshots contain 20 coin symbols each, with BTCUSD
  contract multiplier 100 USD and ETHUSD 10 USD. No U-margined symbols appear.
  A proxy-free public COIN-M metadata request bound to `.10` succeeded.
- Chromium checked both real gateway pages at 1440 and 390 pixels: correct
  coin title, no page errors or horizontal overflow, correct Config links,
  `等待币本位数据`, and equity `--`. Temporary auth sessions were removed.
- Both target deployments have exactly Config and Viz running. Their order
  directories remain empty. All 12 trading TOML hashes and Nginx config are
  unchanged. The only six changed Exec configuration files are the two env,
  Viz and Config files described above.
- 51 other process identities stayed unchanged. Six trade06 processes present
  in the 07:42 baseline were absent afterward. This task issued no trade06
  process commands and did not restart those processes; the stop's cause was
  not established by this deployment verification.

The empty monitor validates the page and transport, not live account equity,
positions, or execution correctness. Those data producers remain stopped under
the operator's instruction. Missing values must not be interpreted as zero.

## Recovery artifacts

Each of `/home/el01/binance_exec_trade10`, `/home/el01/binance_exec_trade11`,
and `/home/el01/crypto_cta_manager` contains
`backups/coin_market_20261008T074912Z`. Exec backups contain the previous six
binaries, scripts, config, and private env file (mode 0600 inside a mode 0700
backup directory). Manager backup contains its prior binary, TOML, and webroot
pointer. Recovery must retain the operator's stopped-trading requirement.

## Independent account observation upgrade

At 08:14 UTC, deployed account-monitor/Viz changes from mkt_signal `6460e8fc`
(on `arbmm`, pushed to origin). The monitor now publishes complete sanitized
STANDARD COIN-M account snapshots; Viz subscribes directly without pre-trade.
The existing CM wallet poll cycle fetches the account endpoint every 5 seconds.
The page separates native-coin assets from factual contract positions and
strategy execution rows. It never sums BTC and ETH quantities or fabricates a
USD account valuation. Failed observations keep their original timestamp;
after 30 seconds the browser indicates delayed account data.

Each target received only the account-monitor and Viz binaries. Config, Manager,
Nginx, other account processes and all 49 audited configuration hashes stayed
unchanged. Existing binaries are backed up in each target's
`backups/account_observation_20261008T081254Z`. Account-monitor PIDs are 419197
and 419432; Viz PIDs are 419071 and 419306. Config retained PIDs 211356/211522.
No pre-trade, trade engine, signal or persistence process was started.
No targets, orders, cancellations, transfers or leverage changes were submitted.

Verification observed advancing snapshots for both namespaces, with 51 asset
rows, zero nonzero assets and zero actual positions, STANDARD account mode.
The authenticated gateways returned coin pages, Config and WebSocket 101.
Chromium at 1440/390 pixels reported `账户监控正常`, zero assets/positions,
no JavaScript errors or horizontal overflow. Temporary sessions were removed.
Unit tests: all 11 account-monitor and 6 Viz tests passed; checks, formatting
and release builds passed. Browser fixtures additionally checked BTC/ETH native
units, signed contract counts, full-snapshot clearing, venue isolation and
staleness. This verifies empty-account monitoring, not live execution.

Artifact checksums:

- account_monitor: `0ce787b6759a269376f6f3ac6dd4bd05110f09664861de1ab1df944b1e07b563`
- viz_server: `10beafe6caa73df285bf2e38a6027bd1a51922d2b4ea56d72076a9059eace7af`

The Viz build also contained a concurrent working-tree log-verbosity change in
its subscriber loop (normal periodic statistics DEBUG, drops WARN). That change
was preserved and excluded from this task's commit. Its isolated build delta is
recorded locally at `.cache/cta-manager-ops/coin_account_viz_build_delta.patch`,
SHA-256 `2fb8414905f17267bce4b059435ec5e9e8c98fa845ca0b4e5d400970ab0c1d52`.
Each target's `ACCOUNT-MONITOR-RELEASE.json` records this provenance.

## NAV and position interpretation

Exec's inverse position ledger conserves contract USD face, converting it to
base-coin quantity at a common reference price. A target is still expressed in
coins, so a fixed coin target differs from a fixed number of contracts.
Manager's rule cache supplies contract size, quantity steps and other limits.
Strategy/source/venue positions remain isolated.

For signed USD face F, inverse coin PnL is `F * (1/entry - 1/exit)`.
Ten 100-USD BTC contracts bought at 50,000 and sold at 60,000 earn
0.0033333333 BTC before fees, or 200 USD at exit. Manager's existing inverse
FIFO matches USD face and reports execution PnL in USD; realized coin proceeds
are translated at each close and are not subsequently marked as wallet coins.
Estimated fees remain face times Maker/Taker rate. Remaining open positions use
the latest fill mark. The factual NAV does not include collateral FX, funding,
deposits/withdrawals or full account-ledger movements. The theoretical Kline
model still supports only USD-M. Neither is a complete COIN-M account NAV.

The new page displays exchange-reported native `walletBalance`,
`unrealizedProfit`, `marginBalance`, available balance and margin requirements.
A future account NAV curve needs a separate native-coin balance history,
valuation marks and external-flow adjustment. These were not silently added to
the existing trade-PnL curve. mkt_signal's living `docs/coin_exec_monitoring.md`
records the read-only transport and unit contract.

## Zero-balance visibility correction

The operator reported no visible data. At 08:28 UTC both live account snapshots
still advanced normally. Browser inspection reproduced zero visible asset rows
because every balance was zero and the default filter hid zero balances.
Checking the existing checkbox revealed all 51 rows. Independent signed read-only
`GET /dapi/v1/positionRisk` requests for each existing API account returned HTTP
200, 30 contract records and no nonzero positions, agreeing with account snapshots.

Deployed Viz `dbe66bf2` at 08:31 UTC. Zero balances now display by default, BTC/ETH
appear ahead of other zero assets, the asset table scrolls without pushing the
position section far down, and a summary shows received and visible asset counts.
The hide/show control remains available. Valid empty asset snapshots are explicitly
distinguished from hidden rows and missing account data.

All six Viz tests and the release build passed. Authenticated desktop/mobile
Chromium checks on both gateways verified 51 visible rows, BTC first, working
zero filtering, advancing account data and no horizontal overflow or JS errors.
Only the two Viz processes restarted; account-monitor and Config PIDs, 57 other
protected process identities, and 49 configuration hashes remained unchanged.
No trading process started. Recovery binaries are in each environment's
`backups/zero_display_20261008T083101Z`; `VIZ-RELEASE.json` records source and hash.
Viz SHA-256: `604fe9626071a6723a39c92158c6c5b3e2e6f340159b23d9807ab87a231537de`.

## Account identity and query-mode audit

After the operator questioned the query scope, additional read-only checks used
each deployment's existing credentials and existing private-account egress.
Both API keys reported `enableReading=true`, `enableFutures=true`,
`enablePortfolioMarginTrading=false`, and IP restrictions enabled.
`GET /papi/v1/account`, `/papi/v1/cm/positionRisk`, and `/papi/v1/cm/account`
returned HTTP 401 / -2015. Those are access failures, not empty-position results.
`GET /sapi/v1/portfolio/account` returned HTTP 400 / -21001 (not a Portfolio
Margin account for that endpoint). The successful ordinary COIN-M account and
position responses establish emptiness only in the queried account scope.
They must not be presented as proof that every possible account mode is empty.

A successful read-only `/api/v3/account` request returned exchange UIDs for both
configured keys. The operator was asked to match those UIDs against the intended
subaccounts; Manager aliases alone cannot establish key ownership. No secrets
were printed or credentials/account modes changed. In particular, the Spot
endpoint's `accountType=SPOT` describes that endpoint and is not used to infer
the global derivatives account mode.

The subsequent read-only USD-M cross-check changed the interpretation:
`GET /fapi/v3/positionRisk` succeeded for both configured keys and returned
**12 nonzero USD-M positions per account**, including BTCUSDT, ETHUSDT and
XRPUSDT. Therefore these API accounts are not globally empty. Earlier wording
about zero balances/positions applies only to the ordinary COIN-M scope.
No USD-M positions were fed into the coin Exec, no targets were published and
no account's trading mode was changed. Resolving whether the intended subaccount
keys or the intended futures market differ requires operator identity/scope
confirmation; keep the independent source/venue boundary intact.

## USD-M correction and latest Manager/Exec publication

On 2026-10-09 UTC the operator corrected trade10 and trade11 to USD-M and
requested the latest Exec for both accounts and bahll202210, with trading
remaining stopped. Trade01 was executing before this task; its environment-local
stop wrappers stopped the deployment before files were replaced. Trade10/11 had
only their three observation services running. Each target was fully stopped
before the publish wrapper's checks and atomic replacements.

All six Exec binaries and current component scripts were built locally from
synchronized production branch `arbmm`, source
`8a263e04ed3190636ba1787ea65e9a9e69c45998`, and published with
`scripts/publish-exec.sh --venue binance-futures --skip-build`. Installed hashes
were checked again after publication. A temporary SSH transport failure before
trade10 publication was retried successfully. Runtime source files had no local
edits; concurrent CME/FR edits were preserved and excluded from task commits.
The local Cargo.lock has concurrent dependency additions; its build hash is
`121321c8f88715f8189a56516d3915ef5e763d9aac3c067c87f31d636cfc00a5`.
Each target's `EXEC-RELEASE.json` records the source, lock hash and six binary
hashes; this full release supersedes the earlier component-only release records.

The two corrected accounts keep their source IDs, namespaces, ports, credentials,
account modes, trading IP bindings and fee settings. Only `EXEC_VENUE`/`VENUE`,
Config venue, Viz venue and the matching Manager source venue change to
`binance-futures`. Trade01's Viz configuration now explicitly declares the same
USD-M venue; its env file is unchanged. Before correction, trade10/11 had empty
order stores, zero Manager snapshot entries and zero strategy bindings. No
historical fill, target or snapshot was relabeled or removed.

Manager release `20261009T080509Z`, source `61fc1cd`, contains the configurable
USDT reserve, automatic Virtual numbering/aliases and corrected editor layout.
The live TOML retains all other settings. Four missing Virtual configuration
tables were added explicitly in one transaction after a full PostgreSQL backup,
using their definitions from the current schema. No automatic migration,
initialization/reset or removal of legacy tables was run.

After publication only each target's `start_account_monitor.sh`,
`start_exec_viz_server.sh` and `start_exec_config_server.sh` ran. No
`exec-pre-trade`, `trade_engine`, `trade_signal` or `persist_manager` was started.
No order, cancellation, strategy target, leverage or exchange-account-mode change
was submitted by deployment checks. Missing execution-state observations while
pre-trade is stopped must not be interpreted as zero factual account holdings.

Verification:

- The six Exec and four Manager release binaries built successfully; all 11
  account-monitor tests, 6 Viz tests and 4 Manager deployment-layout tests passed.
- Manager health reports `ok`, 11 enabled sources, and no refresh error.
  The three target accounts are USD-M in its dashboard. Manager's PID changed
  from 214461 to 939354.
- All three gateways show `U 本位 Exec 执行监控`, return HTTP 200 for page,
  snapshot and Config, and return WebSocket 101. Config bootstraps report
  `binance-futures` with separate source-specific Redis prefixes.
- Each target's current Manager-published USD-M rule snapshot has 920 symbol
  records, includes BTCUSDT and excludes BTCUSD_PERP. All snapshots were refreshed
  recently. This count includes inactive contracts, which remain blocked by status.
- Virtual creation, automatic numbering, alias modification and deletion passed
  against the newly maintained database. The temporary template and auth session
  were removed. The three queried accounts expose `reserve_usdt = 0`.
- Served Manager HTML/JS/CSS hashes match the release; assets are
  `index-D606wdPE.js` and `index-CAgHSgfj.css`.
- 52 other protected process IDs and start times remain unchanged, including
  other accounts, Nginx and the monitor. Of 51 audited configuration files,
  the only changed TOMLs are the three target Viz files and Manager live TOML.
  All 12 trading TOMLs retain their original hashes. The two Config env files
  changed only their venue; private env files changed only the allowed market
  fields, with all remaining content checked for equality.

Recovery backups and verification records use
`backups/usdm_exec_manager_20261009T075516Z` under Manager and each target Exec.
Manager retains the full PostgreSQL dump, exact additive maintenance SQL,
`BASELINE.json`, `VERIFICATION.json`, old binaries/config and old webroot pointer.
Exec backups retain old binaries, scripts, config and privately protected env
files. Credentials were not copied to this repository or logs. Publish staging
files and verification scripts are cleaned after success; recovery backups remain.

## Earn configuration, trade01 restart and retired prc replacement

The operator subsequently authorized trading for bahll202210 only. Its existing
USD-M stack was started through `scripts/start-exec.sh` from synchronized
`mkt_signal/arbmm`, with per-component health checks. Persistence, trade engine,
account monitor, pre-trade, Viz and Config are running; the signal generator
remains stopped. Factual positions are ready, all displayed rows have prices,
and the Manager timeline observed 40 post-restart fill records. No new target,
leverage or account-mode change was submitted by verification. Trade10/11 remain
in observation-only mode.

Bahll202210 Earn retains 8,000 USDT and triggers above 2,000 USDT of available
excess, with a 3,600-second interval and 5,000-USDT automatic round cap. The
operator explicitly chose its existing public trading egress `154.197.32.6`.
`treasury.use_account_ip_rotation = true` resolves existing `0.0.0.0` bindings
through the kernel without packets, then explicitly binds each financial round;
trading TOMLs stay unchanged. Kline retains its separate `.10` address.
The first round subscribed 5,000 USDT into 5,000 BFUSD with zero purchase fee.
On a further manual instruction, a temporary cap allowed another 34,879.04 USDT
subscription, then the automatic cap was restored. Verified futures wallets:
USDT `8000.00042283`, BFUSD `39879.04000000`. This is a point-in-time balance;
live executions and account movements can subsequently change it.

The retired prc deployment (`binance_exec_trade05`) was stopped but had a
569,602,976-byte order store, 5 bindings and both account/strategy snapshots.
Its old RocksDB moved to `binance_exec_trade05/archive/prc/persist_manager`;
historical PostgreSQL rows and archived targets retain their original source ID.
Its active data directory is empty and env.sh blocks accidental starts; historical
credentials remain only in the protected replacement backup. A fresh prepared
deployment `/home/el01/binance_exec_xy_lxy21_05` reuses the trade05 gateway/ports
10045/18165 with a new source ID, namespace and Redis prefix. New API credentials
were validated read-only against USD-M; this account has 15 nonzero positions.
It has zero inherited bindings/snapshots and an empty order store. All six current
Exec binaries were published and SHA-256 verified, with no process started.
The operator explicitly reiterated that xy_lxy21 must remain stopped.
Nginx's existing trade05 auth route now checks the new source ID.

Manager backend `0ce67dd` fixed Treasury route resolution. Subsequent release
`20261009T095926Z`, source `25e2776`, adds Virtual creators and delegated
managers in the API and frontend. Any logged-in user can create an owned Virtual;
editing/deleting requires ownership, delegation or admin status. Only the owner
or an admin manages grants. Account/strategy permissions remain independent.
Legacy ownerless Virtuals remain admin-managed. The additive owner column and
manager-grant table were explicitly maintained in one transaction after a fresh
PostgreSQL backup; startup still performs no DDL.

Validation: 280 crate tests and 6 snapshot-binary tests passed, all 7 isolated
PostgreSQL tests passed, frontend build/lint passed with existing warnings.
Live gateway sessions for ordinary users dzy and shaokai verified separate
creation, mutual grants, delegated edits, denial of further delegation and
immediate revocation. Unrelated edits/deletes were rejected. Verification
templates/grants and temporary sessions were removed. After the final Manager
publication, 63 protected runtime processes, the PostgreSQL/Nginx masters and
all 12 existing trading TOMLs were unchanged. Trade10/11 still have three
observation services each, while xy_lxy21 has no running process.
All four Manager page routes and served HTML/JS/CSS hashes were verified after
correcting the release's `webroot/manager/` directory layout. External gateway
checks also returned HTTP 200. The last trade01 interval check observed 69 fills;
its USDT wallet subsequently read `7998.64875784` following continued executions,
while BFUSD remained `39879.04000000`. The reserve prevents further subscriptions
from spending the retained amount; it does not replenish USDT consumed by trading.
Rollback config, binaries, PostgreSQL dumps and financial results are in
`backups/earn_replace_20261009T093037Z`, including the `virtual_permissions`
subdirectory. Retired prc's private config backup is
`binance_exec_trade05/backups/prc_replaced_20261009T093037Z`.

## xy_lxy21 c40 activation

The operator subsequently requested trading, superseding the earlier stopped
state, and specified `virtual01` (`c40t12_group1`) at multiplier 116. The enabled
source was initially invisible to ordinary users because its fresh identity had
no account grants. Manager's account-grants API now gives shaokai and dzy
`configure` access, which includes visibility. No retired prc permissions were
copied, and neither user's role changed.

The Follow configuration was saved through the Nginx Manager API before starting
Exec. Its two 0.5-share bindings materialized as 58 shares each; the durable
publish queue drained and both complete 40-symbol target vectors were confirmed
in the new account's Redis namespace. The first startup stopped at the missing
new-source risk hash. The existing Exec `sync_exec_risk_params.py` initialized
only that empty hash with its standard five parameters: 10 total live limit
orders, 10 per side, 400 orders/minute and 200 orders/10 seconds. No target or
order-history key was copied from prc.

The synchronized `arbmm` `start-exec.sh` then passed all six component checks.
Persistence, trade engine, account monitor, pre-trade, Viz and Config are live;
trade_signal remains stopped. The standard account-mode gate passed and startup
cancelled the account's existing USD-M orders. Subsequent Viz state reported
positions ready, both named strategies fully allocated and 12 displayed position
rows each. All 24 named-strategy rows completed their current execution; three
system residual rows also reported completion. A source-scoped factual timeline
observed 94 Maker fill records after this successful startup. This is a measured
interval, not a guarantee about later target updates.

Ordinary sessions for both shaokai and dzy verified account visibility, configure
access, Follow state, gateway snapshots and Config bootstrap. All 62 preexisting
protected processes remained unchanged; Manager was not restarted. Trading IPs,
leverage, account mode, old prc archives and PostgreSQL snapshots were preserved.
No manual balance-based snapshot was created: the operator chose live Exec
allocation instead. Recoverable catalog/risk/runtime evidence is under the new
environment's `backups/follow_c40_start_20261009T104355Z`; its `EXEC-RELEASE.json`
records the authorized running state and correct environment identity.

## xy_lxy21 initial inventory and strategy PnL

The operator requested correct initial inventory, independent strategy PnL and
browser verification. Exec already had the live strategy allocation, but Manager
had no account or strategy snapshot for this new source. Backfilling the anchor
from current allocation and later factual fills initially failed for LINKUSDT:
that residual holding had no post-anchor fill to supply a reference price.

The existing `nav_strategy_snapshot --infer-from-fills` tool now accepts explicit
repeatable `--reference-price SYMBOL:PRICE` fallbacks. They must be finite and
positive, with unique nonempty symbols; factual fill prices always take
precedence. This keeps untouched inventory usable without silently treating a
later live mark as its initial price. Source `517dd90` was built locally and
published only as the standalone snapshot tool, SHA-256
`19e28b535f1222e24b4f45837b58efea324680f68894749fc3cc220c139ffd0e`.
Its release record and Manager's root runtime record describe this override.
The running `cta_web` remains `25e2776`; no runtime restart was needed.

After a recoverable backup and a successful dry run, the tool inferred the
initial strategy quantities by subtracting all post-start factual fills from
the current Exec allocation. The immutable anchor is `1791542845000000` us,
2026-10-09 10:47:25 UTC / 18:47:25 Shanghai, before the first factual fill at
`1791542851134220` us. All startup and subsequent executions remain in the PnL
interval. These are inferred initial allocations, not a claim that Exec
captured an allocation snapshot at that historical moment.

The strategy snapshot contains 24 initial lots: 10 for
`CTA_SK_C40V6PosT1_LXY_filter_Position`, 11 for
`CTA_SK_C40V6PosV5_V2_LXY_filter_Position`, and 3 for `__unallocated__`.
Both named strategies retain 58 Follow shares. A matching immutable account
snapshot contains the 15 aggregate symbol/venue positions at the same timestamp
and prices. Existing sub-dollar inference filtering is unchanged. Untouched
inventory uses the last fully closed Binance one-minute candle before startup
(10:46 UTC), retrieved with explicit `154.197.32.9` egress; the backup preserves
the raw candles and price provenance. Fill-derived prices override those
fallbacks wherever available. Neither snapshot modifies Exec RocksDB, strategy
targets, trading bindings, or retired prc history.

After Manager's normal refresh, source-scoped account and strategy timeline
queries agreed on NAV before/after estimated fees and total estimated fees.
Every displayed time point is additive, both initial baselines are zero, and
strategy totals match the account's 514 factual fills in the measured interval.
The two named strategy buckets and unallocated residual are present; no
`SYSTEM_POSITION_CLOSE` strategy bucket is fabricated. The strategy and account
anchors both report the startup timestamp above.

Chromium through the production Nginx gateway checked shaokai and dzy at
1440×1000 and 390×1100. Both users see the independent strategy curves and
nonempty PnL rows, with all three buckets selected. Follow configuration still
shows multiplier 116 and enabled configuration controls. Exec Viz shows both
named strategies and ready factual positions. All four cases had no JavaScript
errors, failed HTTP responses or page-level horizontal overflow. Screenshots
were inspected, then removed; temporary auth sessions and browser staging were
removed after verification.

Formatting, `cargo check`, the full crate suite (280 library tests plus 8
snapshot-tool tests) and the snapshot-tool release build passed. Three library
tests and the seven isolated PostgreSQL integration tests remain intentionally
ignored in this run. All 67 protected Manager/Exec process identities remained
unchanged throughout; trading continues. Recovery snapshots, old binary,
historical candles, dry-run output and API/browser verification are retained in
`/home/el01/crypto_cta_manager/backups/xy_initial_pnl_20261009T124955Z`.
