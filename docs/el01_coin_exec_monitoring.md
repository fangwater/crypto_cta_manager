# el01 coin-margined Exec monitoring

Last updated: 2026-10-08 UTC.

Current state after the operator clarified that the constraint is **no orders**:
both account monitors are running independently alongside Viz and Config.
Execution processes remain stopped. The initial empty-page deployment below is
historical; the account observation upgrade is documented at the end.

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
