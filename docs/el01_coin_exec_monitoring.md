# el01 coin-margined Exec monitoring

Last updated: 2026-10-08 UTC.

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
