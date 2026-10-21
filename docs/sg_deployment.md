# SG Manager deployment

Last verified: 2026-10-20 02:22 UTC.

At 02:21 UTC on October 20, Manager alone was replaced as release
**`20261020T022047Z`**, PID **4092353**, retaining CPUs 0–7. The running
`cta_web` SHA-256 is
`540f51dd576cdfbeb9f0d0d8dd8e1295da0d739ada5d94f87d269bac85b3cb1f`.
The strategy PnL JSON summary and Arrow export previously copied raw TOML
configuration, omitting the PostgreSQL fee overlay and returning 500. They now
read the same immutable NAV generation as the dashboard: resolved catalog fees,
account/strategy anchors and factual fills are captured together. A regression
test covers an empty account anchor overriding older inventory, exclusion of
pre-anchor fills, catalog-only fees, fee refresh and equality with the NAV curve.

The existing real flat-account snapshot **1792418799432000** is active for both
account and strategy PnL. It remains the 2026-10-19 **22:06:39.432 Shanghai**
zero start; no later zero snapshot was invented over live positions. A query
ending exactly at this anchor returns zero for every PnL/count/fee field.
Summary and Arrow requests now return 200. A fixed anchor-to-2026-10-20
02:15 UTC query retained exactly the same account NAV before/after deployment;
the repaired strategy summary equals the existing strategy NAV curve. That
fixed report includes 378 strategy fill events, 16.05592776 USDT before
estimated fees and 8.53532882 USDT after them. Account total is 8.41630813 USDT
after fees, including two separate system-close events. These are order-based
estimates at the report's latest fill marks, not exchange equity or an account
ledger return.

The 02:22 UTC execution check found 19 current numeric targets complete with
only exchange-minimum residuals, fresh quotes/position state and no pending
orders or batches. Counts vary as RX02 publishes. A separate signed account
scan confirmed no open orders, loans or non-USDT spot inventory. All **18**
protected process identities and **26** configuration hashes were preserved;
all catalog hashes except live position targets remained unchanged, including
all four immutable snapshot tables. The publish queue was empty. Frontend,
analysis CLI binaries, trading parameters and all Exec processes were retained.
Formatting, cargo check, 368 Rust unit tests, nine Redis integration tests and
the local release build passed; five unit tests and eleven PostgreSQL tests
retain their explicit infrastructure-dependent ignores. Temporary API sessions
were removed. Evidence: `artifacts/sg_bybit_health_20261020/`; host-only recovery
backup: `deploy-backups/20261020T022047Z-pnl-cache-fees/`.

Bybit `bybit_exec_trade01` is now executing RX02 with **ctaker**. This is the
former arb02 UTA account. The operator requested a complete flatten before
following: six USDT perpetual positions and six spot exposures were closed,
BTC/ETH/BNB liabilities repaid, and six residual spot balances converted to USDT.
Two complete signed, source-bound scans verified no derivative positions, open
orders, non-USDT spot balances or loans. A new immutable empty position snapshot
was appended at **1792418799432000** (2026-10-19 14:06:39.432 UTC); the earlier
12-position snapshot and raw order history remain intact. At the zero anchor,
the account held 19858.44235561 USDT. Evidence is in the private host
`account-flat-baseline.json` and local cleanup JSONL receipts.

Execution uses trade_engine PID **3797810** and exec-pre-trade PID **3819155**,
both on CPUs 0–7; Exec `trade_signal` stays stopped. The pre-trade binary was
built locally from synchronized mkt_signal `arbmm` `bc7223fe` plus scoped changes
to expose the implemented native UTA CLI and select independent public IPC roots.
SHA-256: `cd77932fd216af9798da842fc4f273ba57f8d210b9e6d608afddb9728ed9d735`.
Account mode, trading IP `172.31.7.124`, order template, risk limits and shares
were preserved. The account is Portfolio Margin; no fixed leverage was changed.

The old shared public processes retained orphaned IPC mappings, preventing new
subscribers from joining. A dedicated read-only public feed now runs at
`/home/ubuntu/spread_pbs_cta/bybit-futures`, PID **3818626**, CPU **6**,
publishing BBO and derivatives for all **798** active Bybit USDT perpetuals.
Its REST and WebSocket clients bind data ENI **172.31.10.217**. Both publisher
and Exec use `MARKET_DATA_IPC_NAMESPACE=bybit_exec_trade01`; services are
`bybit_exec_trade01_spread_pbs/bybit-futures/ask_bid_spread` and
`bybit_exec_trade01_dat_pbs/bybit-futures/derivatives`. Existing shared publishers
997930/997951 and all MM processes were preserved. No global IPC files were
removed or reconstructed. The dedicated feed has environment-local start/stop
scripts with a distinct supervisor name and exact binary process matching.

The 15:06:59 UTC exchange audit found **33 fills**, reconciling exactly to the
then-current 12 perpetual positions from the zero anchor. All active targets
were complete, with residuals below exchange minimums, no pending orders,
no spot balances and no loans. Subsequent RX02 targets and `clear` commands
continue to execute. Target counts change during live trading: 47/47 symbols
match; unit multipliers and shares remain 1. The JP/SG target hashes matched.
Manager reads the new durable fills from Exec RocksDB; its final dashboard
check reported 18 fill events and the exact empty snapshot above. Authenticated
Manager/Viz/Config checks returned 200, WebSocket returned 101, and the temporary
session was removed. Native UTA CLI tests,
market namespace tests, 1312 library tests, 15 publisher tests, six wrapper
tests and local release builds passed; three unrelated library tests were ignored.

Other limitations observed during verification: reload-notify IPC is
corrupt, so successful Redis target writes are picked up by the configured
30-second poll. The strategy PnL fee lookup failure was repaired on October 20
as described above. The dashboard reads the
persisted fills and the new zero snapshot successfully. Daily USD-equity/USDT-NAV
reconstruction also retains its prior result on currency incompatibility.
Manager currently classifies these Bybit durable events as unknown liquidity;
the independent signed exchange audit confirms Market orders with isMaker=false.
No fee settings were changed to work around these analysis issues.
Runtime manifests, dry-runs, installation receipts, signed execution evidence
and protected-process checks are in `artifacts/sg_target_proxy_20261019/`.
Binary/environment backups are under host-only `deploy-backups/`, including
`20261019T145758Z-exec01-market-runtime`. No remote Cargo build or database DDL ran.

The target-proxy release was **`20261019T132344Z`**, built from checkout `0e419f5`
with the locally validated target-proxy changes. The release manifest explicitly
records the dirty working tree. Four Manager/analysis binaries and every frontend
file passed SHA-256 verification, and the running `cta_web` matches
`d21174e031bee423d8a8bb44af21050e179bf50f4cf1d97ad88b5c5da6020504`.
Only Manager restarted on SG, as PID **3783225**, retaining CPUs 0–7.
`bybit_exec_trade01` now enables `[sources.target_proxy]`; its shares and all
account, Virtual, risk, permission, fee and snapshot settings are preserved.
The refreshed Bybit cache matched 47/47 live RX02 symbols, with unit ratios 1
and no filtered symbols. See [target proxy](bybit_target_proxy.md).

At **13:37 UTC**, JP's `binance-cta-perp-rx02` position publisher was updated
to the previously validated multi-destination binary
`dc09fce1c0d9f11166c1b6b6cf4704c68b95a4e30512afcd3593bbfb7c8bd41d`.
Only `cta_perp_signal` restarted, from PID 100371 to **3639659**; its saved
position file was retained without changing quantities. It now publishes
independently to the existing JP gateway and SG's
`http://47.131.162.78:4191/manager/api`, with separate SG token authorization.
Credentials remain in host-only mode-0600 files and are absent from artifacts.
JP's RX02 trade engine PID 95314 and pre-trade PID 95709 were unchanged.
The factor publisher, configuration server and dashboard were also unchanged.

Both destinations reported successful delivery with no error or pending strategy.
A read-only comparison found identical 47-symbol target vectors on both Managers:
9 numeric nonzero targets and 38 `clear` targets at that check. SG's Redis runtime
matched its catalog. Its immutable archive contains the same account-specific
projection at shares 1, with a 12.6-second-old rules snapshot. Authenticated
health, account feed, account catalog, target preview, Manager page, Exec Viz,
snapshot and Config checks returned 200; WebSocket returned 101, anonymous
health remained 401, and temporary verification sessions were removed.

All 15 protected SG process identities and all 15 catalog/snapshot table hashes
were preserved during the Manager update. After live publication, position
strategy targets/timestamps change as expected. Only the SG Manager TOML and
JP RX02 publisher's environment were intentionally changed. A concurrent JP
trade06 pre-trade process change was observed outside this task; this operation
did not invoke any trade06 wrapper or modify its configuration. At that 13:50
checkpoint Bybit execution remained stopped; its subsequent authorized flatten
and ctaker activation are documented above.
Nginx, monitor, Kline and funding settings are unchanged; no database DDL ran.

Recoverable backups remain on the respective hosts at
`~/.config/crypto-cta-manager/deploy-backups/20261019T125745Z-target-proxy/`
and `~/.config/crypto-cta-manager/deploy-backups/20261019T133713Z-rx02-sg-publisher/`.
Evidence: `artifacts/sg_target_proxy_20261019/`. HTTP proxy transport and direct
SSH were used for the completed transfer/verification after the operator
explicitly prohibited SSH jump hosts. Task wrappers contain no jump route.

## Earlier preparation and snapshot (08:28 UTC)

The earlier release was `20261019T080045Z`, checkout `6e422e1` (runtime and
frontend code `ac9c997`; the later commit changes documentation only).
Four Manager/analysis binaries and all frontend files were built locally,
uploaded, SHA-256 verified and atomically installed. During the 08:01 UTC
upgrade only Manager restarted, as PID 3647035 with affinity 0–7. Its later
restart for the Exec 01 source switch is recorded below. All 15 protected process
identities, 21 live configuration hashes, existing strategy/snapshot catalogs,
LGQ permissions, virtual ownership and follow settings were preserved.

Explicit database maintenance added the previously absent
`cta_order_sources.quote_fee_rates` column and `cta_account_equity_daily`
table, owned by the application role and using Shanghai-midnight boundaries.
There was no pre-existing daily table to replace and no historical-row deletion
was needed. The prior binaries, unit, live config, frontend pointer and a full
PostgreSQL dump remain on SG only under
`~/.config/crypto-cta-manager/deploy-backups/20261019T080013Z-manager/`.

Authenticated LGQ health, account/catalog, account-overview, Manager pages,
Exec Viz/snapshot and Exec Config checks all returned 200; the existing Exec
WebSocket returned 101 and anonymous health remained 401. The Bybit unified
account feed is healthy, the configuration publication queue is empty, and
all four observed account-monitor sockets still bind `172.31.7.124`.
The new overview reports factual current equity and initial margin. Daily
equity/profit, exposure and historical drawdown remain unavailable. The current
daily reconstruction worker supports native Binance USD-M only; the new Bybit
account snapshot below does not enable Bybit daily equity reconstruction.
Kline downloads, funding-rate downloads and the standalone monitor stay disabled.

At the operator's request, the original intra arb02 account now uses
**`bybit_exec_trade01`**, alias **`bybit-01`**, replacing the prepared Exec 02.
LGQ has configure permission and follows its existing `virtual01 / lgq_cta`
with multiplier 1, ten bindings and one active binding. Order parameters and
all five risk fields were preserved. The old source is disabled, detached from
follow and has zero shares; its source identity and historical files remain.
No order archive or old-order import is required. Both original intra stores
and the prepared Exec 02 store remain in their original directories.

A new immutable account snapshot was stored at **2026-10-19 08:22:01.653 UTC**
(`1792398121653000` microseconds), covering six spot net positions and six USDT
perpetual positions. The two venue ledgers remain separate. Spot quantities use
Bybit wallet balance minus spot borrowing; BTC, ETH and BNB are net short,
while SOL, XRP and DOGE are positive holdings. USDT cash, raw balances, borrowing,
interest and account totals are retained in the immutable snapshot note and
`/home/ubuntu/bybit_exec_trade01/account-baseline.json`. Both complete position
and net-wallet scans agreed and spot/perpetual open orders were zero.
`nav_rebuild` reproduced all twelve initial lots with zero historical fills,
zero fees and zero initial NAV change. Existing inventory has only
`__initial_position__` attribution, with no invented CTA allocation.

Native Bybit Exec order execution remains limited to USDT perpetuals. Recording
factual spot inventory does not add spot order execution or ongoing spot-fill
collection. NAV remains an order-only change from the snapshot, not absolute
account equity; future borrowing interest and other ledger movements are not
part of order FIFO. Bybit daily equity/profit reconstruction is not supported
by the current worker; current equity and initial margin work through the
unified-account feed. The RX02 live publisher is still not connected to SG.

The four observation services now run in `bybit_exec_trade01` on CPUs 0–7:
Config PID 3656055, Viz 3656122, persist_manager 3656242 and account_monitor
3656401. Manager restarted for the source change as PID 3656412, also on 0–7.
`exec-pre-trade`, `trade_engine` and `trade_signal` remain stopped, as do both
intra environments and all old Exec 02 processes. The eleven unrelated MM and
public-data processes retained their identities; all twenty pre-existing
non-Manager configuration hashes were unchanged. All four new account-monitor
sockets bind `172.31.7.124`; ENIs, routes and isolated cores are unchanged.

Authenticated `/exec_trade01/`, `/exec_trade01/config/` and
`/exec_trade01/snapshot` return 200; WebSocket upgrade returns 101. Viz/Config
listen only on `127.0.0.1:10041` / `127.0.0.1:18161`; old ports 10042/18162
are closed. Manager health and account feed are healthy, publication queue is
empty, and anonymous health remains 401. Gateway templates now use Exec 01.
The six Exec binaries are the previously verified local production build from
mkt_signal `a73d427d`; their exact published SHA-256 hashes were checked before
copying atomically into the new environment. No remote Cargo build ran.
Cutover backups, including a full PostgreSQL dump, remain on SG only at
`~/.config/crypto-cta-manager/deploy-backups/20261019T081311Z-exec01/`.
Cutover and snapshot evidence is in `artifacts/sg_exec01_20261019/`.

Verification evidence is in `artifacts/sg_manager_20261019/`. Local checks passed
362 Rust unit tests, 11 isolated PostgreSQL integration tests and 23 frontend
tests, plus formatting and the production frontend/release builds.

Independent installation at `/home/ubuntu/crypto_cta_manager`, reachable through
`http://47.131.162.78:4191/manager/`. Backend listens only on `127.0.0.1:18201`.
PostgreSQL 16 is local on `127.0.0.1:5432`; Redis uses `127.0.0.1:6379/0`.
Manager and PostgreSQL use housekeeping CPUs 0–7. The existing trading/IRQ
cores 8–15 and unrelated trading processes are unaffected.

At the previous deployment, binaries and frontend were built locally from `d2248e7`, uploaded to
explicit temporary names, SHA-256 verified and atomically installed as release
`20261016T102806Z`. The release
manifest is `webroot/RELEASE-MANIFEST.txt`. Service:
`systemctl --user status crypto-cta-manager-web.service` (enabled).

Database credentials stay on SG in
`/home/ubuntu/.config/crypto-cta-manager/database.env` (mode 0600). Never copy
credential values or password hashes into Git, logs or chat. The initial
administrator was created via loopback before exposing the gateway.

On 2026-10-16, at the operator's request, SG's initial `admin` was replaced with
jp-meta's active administrator `prc`, using the same stored password hash and
retaining SG's local user ID. The password therefore matches jp-meta. Only the
administrator authentication record was copied; jp-meta was read-only. SG's old
sessions were revoked, and the obsolete `bootstrap-admin.json` was removed from
the active credential directory. A restricted authentication backup remains in
`~/.config/crypto-cta-manager/auth-backups/20261016T095004Z/` (directory 0700,
files 0600). Password-hash equality was verified without printing it.
Authenticated administrator/health checks through Nginx passed using a temporary
SG session, which was then removed; anonymous health requests return 401.
The Manager process was not restarted, and no trading configuration changed.

The historical 2026-10-16 preparation used `bybit_exec_trade02` for lgq with the alias `bybit-02`,
an independent namespace and a new empty Exec persistence store. Its six Rust
binaries were built locally from mkt_signal `a73d427d`, uploaded, SHA-256 checked
and atomically installed. `bitget_exec_trade01` remains reserved and disabled.
Kline, funding-rate downloads and the standalone alert monitor remain disabled.
Public requests use the data ENI; account requests bind `172.31.7.124` explicitly.

Bybit's account monitor, persist_manager, Viz and Exec Config run on CPUs 0–7.
`exec-pre-trade`, `trade_engine` and `trade_signal` are **stopped**. Do not run the
full start wrapper as an observation check: pre-trade startup cancels orders.
The existing intra environments remain stopped and their config/positions/history
were not migrated into Exec. The new env securely copies only the existing
arb02 Bybit credentials and trading configuration; all private sockets were
verified on `172.31.7.124`. No account mode, leverage or position was changed.

The Manager lgq login uses the same password hash as JP, with ordinary-user
configure access only to this source. It owns SG's `virtual01 / lgq_cta`, which
has the same ten bindings and multiplier 1 as JP RX02's `virtual02`. The sole
active binding is `lgq_cta_symbol_mean` at 1 share; the other nine stay at 0.
The `lgq_cta_ctaker` order template and all five Exec risk fields were compared
with JP's live `binance_exec_trade05`: ctaker, 100 USDT per order, one order per
batch, max_batch 1, rate limits 400/minute and 200/10 seconds. Initial targets
are numeric zero, and the live RX02 publisher is not connected to SG.

Manager now preserves RX02's explicit `qty: "clear"` contract through parsing,
catalog storage, immutable target archives, shares scaling, Redis publication,
Exec Config readback and the target editor. Numeric zero retains its separate
meaning. No database schema change was needed. The private future publish token is held only in
`~/.config/crypto-cta-manager/bybit02-publish.env` (0600).

An authenticated publication through SG's Nginx gateway changed the prepared
`lgq_cta_symbol_mean` BTCUSDT target to `clear`, confirmed that the catalog and
archive retained signal -1 while the disabled signal-execution setting emitted
signal 0 to Exec, and read `clear` back through Exec Config. An invalid `close`
command returned HTTP 422 without changing the target. All original numeric-zero
targets were then restored through Manager. These two accepted publications
remain as factual audit messages; no execution process was started or exchange
order submitted. The live RX02 publisher remains unconnected to SG.

Local validation passed 349 Rust tests, the independent Redis integration test
(including native Bybit and Bitget clear publication/readback), seven Python
publisher tests, 18 frontend tests and the production frontend build. All four
installed Manager binary hashes and the running executable match the release
manifest. Ten protected process identities, trading configuration hashes,
binding/follow settings and risk/order parameters were unchanged. The temporary
verification session was removed.

At that historical preparation, authenticated `/exec_trade02/`, `/exec_trade02/config/` and snapshot routes
return 200; its WebSocket upgrade returns 101. Anonymous requests return 401.
Manager's Bybit unified-account feed is healthy, all 47 RX02 symbols are active
in the refreshed Bybit rule cache, and the configuration delivery queue is empty.
Viz/Config listen only on `127.0.0.1:10042` / `127.0.0.1:18162`.
Deployment backups are under
`~/.config/crypto-cta-manager/deploy-backups/20261016T100107Z-bybit02/`; parameter
and process verification is recorded in mkt_signal's
`artifacts/sg_bybit02_exec_deploy/20261016T100107Z/validation.json`.

For updates, build locally then run:

```bash
scripts/deploy_host.sh --target sg --manager-only --skip-build
scripts/install_jp_meta_nginx.sh --target sg
```

The publisher leaves live config intact and uploads a `.template`. The gateway
mapping uses one `external:` snippet, preserving SPA routes when the site's
configuration is regenerated. Initial gateway backups are in the runtime root.
First installation requires PostgreSQL packages, the host-guarded
`scripts/install_postgresql_cta_sg.sh`, and an explicit `cta_web --init-db` with
the database environment loaded before starting the service. Never initialize
an existing database or overwrite the credential files.
