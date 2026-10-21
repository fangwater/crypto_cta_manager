# Grid order templates

Manager supports Grid templates for RapidX and supported native unified accounts.

## Japan template split, 2026-10-21

At the operator's request, the former `lgq_grid` was split into two templates,
both retaining `lgq` ownership. RX01 (`virtual03` / `binance_exec_trade06`)
uses `lgq_grid_spread`; RX02 and RX03 (`virtual02` / `virtual04`, sources
`binance_exec_trade05` / `binance_exec_trade07`) use `lgq_grid_prep`. The
operator's exact spelling `prep` is retained. RX02/RX03's active targets are
perpetuals only; no market-routing or position-target changes were introduced.

| Template | Spot | Futures |
| --- | --- | --- |
| `lgq_grid_spread` | 5 levels, 120 seconds, one Maker round | 5 levels, 30 seconds, unlimited Maker rounds |
| `lgq_grid_prep` | Same unchanged spot parameters | Copied from spot: 5 levels, 120 seconds, one Maker round |

Both retain single-order size 100 USDT, volatility multipliers 1 and the existing
signal execution switch. After a finite Maker round the remaining quantity uses
the existing Taker fallback. All 74 bindings retain their shares and follow
multipliers; 64 remain active and 10 stopped. Manager published and verified all
148 market-scoped runtime configurations with no pending deliveries. The old
template was removed only after every real/Virtual reference was replaced.
Recoverable catalog evidence is in the host-local
`/home/ubuntu/crypto_cta_manager/backups/grid_split_20261021/` directory.

RX01's stalled position display was repaired at 08:14–08:15 UTC by replacing
its executor and Viz together. The single existing state channel now permits
1 MiB complete snapshots, retains two history samples and allows an eight-sample
subscriber queue. The compact payload and HTTP/WebSocket format are unchanged.
The binaries were built locally from synchronized Exec `arbmm` base `115213a6`
plus the recorded state-channel patch; the host evidence includes exact hashes.

Three samples ten seconds apart returned fresh, ready snapshots with 1,506–1,509
rows across 59 strategies, including the system residual strategy, at 330–513 ms
age. Both cash and perpetual rows are present and all displayed algorithms are
Grid. The previous oversize-message errors stopped. Four cash strategy rows
still report `borrow_unavailable` (RLC, IOST and two KNC rows); this is a separate
execution constraint. Live KSM/QTUM orders also showed temporary differences
between account quantities and summed strategy allocations during observation,
so monitoring recovery does not certify every target as reached.

Validation passed 1,342 Exec library tests (3 ignored), 2 codec tests, 6 Viz tests,
format/check and release builds. The regression fixture preserves all fields in
a 6,600-row mixed-market snapshot. Only RX01 executor/Viz PIDs changed, to
502412/502880, both still on CPUs 0–5; 134 other observed process identities and
118 configuration hashes were preserved. Gateway and WebSocket checks passed.

## Japan activation, 2026-10-21

At 07:51 UTC the operator activated the existing `lgq_grid` template for all
bindings in RX01, RX02 and RX03. Manager's Nginx API updated their followed
Virtual accounts; no process or binary was replaced.

| Account | Source | Virtual | Bindings | Active |
| --- | --- | --- | --- | --- |
| RX01 | `binance_exec_trade06` | `virtual03` | 59 | 58 |
| RX02 | `binance_exec_trade05` | `virtual02` | 10 | 1 |
| RX03 | `binance_exec_trade07` | `virtual04` | 5 | 5 |

All 74 bindings now select `lgq_grid`. Shares, follow multipliers and stopped
bindings are preserved. The operator's template is unchanged: single-order size
100 USDT, five levels in each market, volatility multiplier 1; spot uses one
120-second Maker round, futures use unlimited 30-second Maker rounds. Existing
signal-driven Taker behavior remains enabled.

Verification at 07:55 UTC found no pending configuration deliveries, all 148
market-scoped runtime configurations using the template, no target mismatches,
and unchanged observed process identities. All three Exec logs confirmed their
strategy configuration reloads. Read-only recent RocksDB inspection confirmed
post-switch exchange fills for RX01/RX02 without rejected orders. RX03 initially
had no executable delta; a new DASHUSDT target at 07:56 UTC subsequently produced
Grid limit orders and two fill events by 07:57 UTC, also without rejections.
Signal status files were fresh and reported no publication errors.

RX01 has a pre-existing monitoring fault: its strategy-state payload is about
140 KB, exceeding the 64 KiB IPC limit. Its Viz strategy snapshot is approximately
42 hours old and still displays CTaker. Runtime Redis, configuration reload logs
and fresh order records independently confirm Grid execution; the old snapshot
cannot verify current full-position convergence. This activation does not repair
that fault or restart trading services.

Recoverable configuration and verification evidence remain host-local under
`/home/ubuntu/crypto_cta_manager/backups/grid_activation_20261021/`.
Temporary authentication sessions and the read-only diagnostic executable are
removed after verification.

## Template parameters

The form has one shared single-order size and the same controls for each market:

| Field | Spot default | Futures default |
| --- | --- | --- |
| `vol_multiplier` | 1.0 | 1.0 |
| `orders_per_batch` | 3 | 3 |
| `maker_timeout_ms` | 1000 | 1000 |
| `max_maker_requotes` | 2 (3 total maker rounds) | `null` (unlimited) |

Each market independently supports finite maker rounds followed by one taker for
the remaining quantity, or unlimited maker requotes. The UI counts the initial
maker round: N rounds is stored as `max_maker_requotes = N - 1`; `null` is the
only unlimited value. A limit of zero means one maker round. Finite limits must
be below uint32 max. Switching to taker waits for all previous orders' terminal
reports and deducts partial fills. Data-staleness pauses do not consume rounds.
The existing signal execution switch still allows explicit taker instructions.

Multipliers must be finite and positive. Each side's offsets run from zero to
`vol * vol_multiplier`, evenly spaced across its market's split count; one
level has zero offset. The raw volatility formula and freshness rules are unchanged. The
execution parameters depend only on the target's market: spot/perpetual pairs
and standalone perpetual strategies both use Grid without spread membership
checks. Each market still requires its own ready volatility feed. Vol discovers all
symbols in the two Binance Kafka market feeds, independently of Spread membership;
new symbols become ready after their own complete history window.

New order templates belong to the logged-in creator. The catalog returns
`owner` and session-specific `can_configure`; only the owner or an administrator
can save or delete an owned template. Existing templates without a creator stay
shared. Ownership does not change template visibility or account binding rights.
Renaming in the editor creates a new template owned by the current user.

For an existing Manager database, explicitly apply
`scripts/apply_grid_order_schema.sql` and
`scripts/apply_order_strategy_ownership.sql` before installing the new Manager.
Databases with the initial draft Grid JSON must explicitly apply
`scripts/apply_grid_market_parameters.sql`. Run the same explicit maintenance
for existing per-market Grid JSON before installing these controls: it adds
missing multipliers as 1.0 and missing futures limits as `null`, preserving
existing split counts, durations, spot limits, and any already-set controls.
It is idempotent and leaves catalog timestamps and other parameters unchanged. Fresh databases use `migrations/schema.sql`; startup never
performs DDL.

Grid parameters are emitted to Exec only when the selected algorithm is Grid.
A Grid symbol override includes its complete Grid settings when its base uses a
different algorithm. Saving an unbound template only updates the Manager catalog;
activating it requires a compatible Exec and its volatility producer.

The Grid editor sizes its columns to the parameter panel rather than the whole
viewport. Spot and futures use matching field order, aligned controls and a
persistent round-count field (disabled for unlimited mode). Descriptions sit
outside field rows so switching modes does not move the next market section.
Desktop and mobile checks at 1440, 1024, 768, 390 and 320 px verified no horizontal
overflow and stable section positions.

## Japan full-symbol Vol and layout deployment, 2026-10-21

Release `20261021T064822ZGridAllSymbols` uses Exec `d3b724af` and Manager
frontend `82f58cb`. It publishes every symbol carried by the Binance futures and
spot Kafka feeds, independently of the 48-symbol Spread universe. New symbols
join automatically and become usable after their Vol window is ready. Each Vol
stream now retains 4096 messages and each executor subscribes with that capacity.

The replacement publisher replayed in the background from 06:54 UTC while the
existing publisher continued serving Spread/Vol and owning the history files.
The warmed replacement took over at 07:20 UTC. Only one publisher remains:
`cta_spread_factor_binance_futures`, PID 330224, all threads on CPU 17 at normal
priority. Exec05/06/07 were replaced through their environment scripts; PIDs
384442/384533/384621 retain CPUs 0–5. The handoff sequence took 51 seconds,
including the first executor's supervisor shutdown wait; the publisher ownership
check completed four seconds after the old publisher stop returned. A two-second
continuity sample spanning the handoff saw no status read errors, maximum status
age 1920 ms and maximum observed Vol counter progress gap 8002 ms.

Both live streams passed complete-market coverage checks: 524/524 futures and
362/362 spot symbols had ready Vol younger than 15 seconds. The probe also retained
one stale historical spot symbol, EPICUSDT; it is not counted as current coverage.
Every active strategy target passed independently: RX02 has 51 futures symbols,
RX01 has 54 futures and 55 spot symbols, and RX03 has 14 futures symbols. All three
executors subscribed to both streams. Decode, evaluation and publication errors
remained zero. Current algorithms remain CTaker; this deployment does not activate
Grid or alter targets, bindings or the saved Grid template.

The layout uses the actual editor width for responsive field columns. Finite and
unlimited Maker controls keep the same row geometry. Production-build Chromium
checks passed at widths 1440, 1024, 768, 390 and 320 without horizontal overflow.
The deployed HTML and assets were SHA-256 checked through Nginx. Only frontend
files changed in Manager; backend PID 166260 on CPU 24 and all Config/Viz/trading
engine processes were preserved. All 156 protected process identities, 53
unchanged configuration hashes, 12 templates, 87 real-account bindings, 77 virtual
bindings and four snapshot tables passed verification. The remaining config file
changed only by adding the factor-specific process name to its `env.sh`.

Validation: 1341 Exec library tests passed (3 ignored), 3 probe tests, 4 handoff
script tests, release builds, frontend build and browser layout checks. Ten
Manager/Exec gateway checks passed and temporary auth sessions were removed.
The release retains `RELEASE-MANIFEST.json`, `DEPLOYMENT-VERIFICATION.json` and
`UI-VERIFICATION.json`; deployment staging, backups, probes, screenshots and the
browser profile were removed after verification.

## Japan deployment, 2026-10-21

Release `20261021T053158ZGridControls` uses Manager commit `d5c3cc1` and Exec
`arbmm` commit `bf9d0ad8`. Manager API/frontend, RapidX Exec05/06/07 executors
and Config services, and RX01's shared factor publisher were replaced. Manager
was rebuilt from the committed sources in the existing checkout; unrelated local
work was saved and restored byte-for-byte. No trading engine, account monitor,
persist_manager, signal, Viz, Nginx or standard-account Exec was restarted.

The explicit Grid JSON maintenance preserved all 12 templates' existing settings,
including `lgq_grid`'s operator-set one spot Maker round. It added multipliers of
1.0 and unlimited futures rounds. New templates default to three spot rounds.
All 87 real-account bindings, 77 virtual-account bindings and four snapshot tables
are unchanged; `lgq_grid` remains unbound. This deployment enables configuration
and execution support, without switching current live strategies to Grid.

All 152 other long-running process identities and 54 configuration hashes were
preserved. Manager remains on CPU 24, the shared factor publisher on CPU 17, and
Exec05/06/07 on CPUs 0–5. The separate NAV collector's finite `sync_history` jobs
finished normally during verification and were excluded from the process check.

The gateway returned HTTP 200 for all seven Exec Viz, snapshot and Config paths.
A temporary unbound template saved and reloaded spot/futures multipliers 0.5/2.0,
unlimited spot with one futures round, then three spot rounds with unlimited
futures. The temporary template and verification sessions were removed.
Manager briefly returned 502 during its initial report load and recovered before
API verification. Committed-source validation passed 366 Manager library tests
(5 ignored), 9 binary tests, release build and frontend build.

The shared factor publisher resumed output at 05:53 UTC after replaying its
configured three-day Kafka lookback. The restart interrupted its existing Spread
output for about 20 minutes; this replay was caused by including the raw-Vol
producer update, not by the parameter controls themselves. Both spot and futures
Vol probes reported 48/48 symbols ready while receiving fresh messages, and all
three executors subscribed to both streams. Decode, evaluation and publication
error counters remained zero. At this earlier release, Vol coverage was limited to the configured 48
symbols; the later full-symbol deployment above removes that limit.
Final process, configuration, binding and snapshot checks passed. The release
contains `RELEASE-MANIFEST.json` and `DEPLOYMENT-VERIFICATION.json`; temporary
staging, probes and deployment backups were removed after verification.
Manager, Exec and Exec Config must be deployed together for this single contract;
older Exec versions reject the new fields.

Validation: Manager full crate tests (377 library and 9 binary), 15 isolated
PostgreSQL tests, frontend production build, Exec library tests (1,338 passed,
3 ignored), and 47 Exec Config Python tests. Coverage includes independent
multipliers, finite/unlimited rounds in either market, terminal-confirmed taker
fallback after partial fills, symbol overrides, and idempotent catalog maintenance.

## Japan deployment (2026-10-20, before these controls)

Updated 2026-10-20 UTC. Japan Manager release `20261020T131400ZGridMarkets`
uses code commit `73a27c4` and binary SHA-256
`9d3201bda97b68b44f4ab5a9907abac157189dcfdf79da0860d923c5c311c93d`.
The `lgq_grid` template belongs to `lgq` and remains unbound. Its original
single-order size, split count and maker duration were retained as the starting
values for the two independent market configurations.

Browser verification through the Japan Nginx gateway saved and reloaded distinct
spot/futures split counts (2/5) and maker durations (2100/9000 ms) on a temporary
lgq-owned template, then deleted it. The original template was unchanged.
Desktop and mobile rendering passed; volatility controls and the futures maker
retry input are absent. Unsupported volatility fields and futures retry limits
are rejected by the API. Existing 12 templates' other fields, 87 account bindings,
97 trading/gateway process identities, host configuration and Manager CPU 24
were preserved. Only Manager was restarted; Exec/factor deployment remains pending.

Validation: 365 Manager library tests and 9 binary tests, 14 isolated PostgreSQL
tests, frontend production build; 1,336 Exec library tests and 46 Python Config
tests. Manager runtime serialization preserves the two market settings; Exec
selects them by market and ignores edits to the inactive market while orders
remain working.
