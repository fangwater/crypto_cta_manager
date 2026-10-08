# NAV immutable history reuse — 2026-10-08

The dashboard previously rebuilt sorted factual fills and every hourly FIFO
checkpoint on each refresh, retaining two independently allocated generations
while requests finished. Immutable PostgreSQL snapshots did not prevent this
repeated derived-state work.

## Implementation

- Keep a compact shared analysis projection of decoded uniform orders. Raw order
  payloads remain solely in Exec RocksDB; analysis retains the fields needed for
  NAV, acquisition cost, and source-symbol indexing.
- Share persistent FIFO lot queues, state maps, sorted fill vectors and immutable
  checkpoints between dashboard generations.
- An unchanged source reuses its prepared FIFO state. A normal append replays
  only new fills. A late fill or legacy exchange liquidity correction resumes
  from the last unaffected hourly checkpoint and reconstructs its suffix.
- Changed fees/snapshots, a replaced source database, or a skipped history
  generation safely rebuild that source. A late fill that could change an
  inferred initial reference price also triggers a full source rebuild.
- Keep old generations immutable for concurrent requests. Reuse bounded query
  results only when all source objects are unchanged. Manual and periodic
  refreshes continue to share the existing refresh lock.

There are no PostgreSQL schema changes, durable derived caches, Exec RocksDB
writes, or financial-formula changes. Initial startup still reads factual order
history. The theoretical archive index and its cold-start scan are unchanged;
this change targets factual FIFO refresh CPU and retained memory.

## Verification

`cargo fmt --check`, `cargo check`, and the full `cargo test` suite passed:
255 passed and 7 ignored. New equivalence tests compare incremental dashboard
and timeline results with full recomputation across allocation modes, symbols,
hour boundaries, late fills, raw Maker/Taker corrections, explicit fill roles,
fee/snapshot changes, skipped generations, and source database replacement.
`npm run build` passed for the deployment frontend.

The release benchmark uses 60,000 fills over approximately 166 hours, three
symbols and four strategies, retaining two dashboard generations. All lots are
open, deliberately stressing checkpoint retention. The pre-change executable
was preserved locally before compiling the new implementation. Measurements
are a synthetic process benchmark, not an estimate of total production RSS.

| Synthetic release benchmark | Before | After |
| --- | ---: | ---: |
| First build | 210 ms | 96 ms |
| Unchanged-history refresh | 170 ms | <1 ms |
| Two-generation process RSS/HWM | 462,148 KiB | 56,068 KiB |

RSS decreased by 87.9% for this fixture. Runtime allocation benefits depend on
open lot counts, history size, strategies, and concurrent queries.

## el01 deployment

Deployed runtime `1a3f88a` using `--manager-only` as release
`20261008T070944Z` under `/home/el01/crypto_cta_manager`. The prior Manager was
stopped before this deployment. Recovery artifacts are under
`backups/nav_cache_20261008T070852Z`; the previous frontend release is
`web-releases/20261008T062132Z`.

The gateway health response reported `ok`, 11 sources, and no refresh error.
All 11 enabled sources returned 98 factual points for a fresh one-day range;
repeat reports were identical. First requests took 28–555 ms over the local
Nginx gateway, repeats 27–345 ms. Temporary verification sessions were deleted.

The first periodic refresh completed in 8,959 ms including RocksDB scans,
symbol indexes and PostgreSQL work. Runtime FIFO logs showed only 104 of
359,073 fills replayed for trade03, 71 of 288,534 for trade06, and 157 of 675,744
for trade07, in both allocation modes. Before/after checks found all 41 protected
process identities and 14 configuration hashes unchanged. This includes the
12 trade-engine TOMLs, Manager live TOML and gateway configuration.

After factual queries and the first refresh, Manager RSS/HWM was 5,183,724 KiB
(4.94 GiB), with zero process swap. There was no live before/after memory
comparison because the old Manager was already stopped.

### Kline enablement

The operator subsequently authorized enabling Kline on el01. Per the updated
AGENTS.md, the configured binding `154.197.32.10` was verified with a bound,
proxy-free public-IP request and Binance server-time request. All 12 trading
TOMLs use unspecified local bindings, resolved without packets to the default
local/public route `154.197.32.6`. Both that address and `.9` remain excluded.
Only `kline.enabled` changed from false to true; retain 30 days, concurrency 8,
weight budget 120/minute, and the five default symbols remain configured.
The original config is backed up at
`config/backups/kline_enable_20261008T071524Z/cta-manager.toml`.
Manager alone was restarted once more for this separately requested change.

Before Kline initialization, the root filesystem was 44% used with 53 GiB
available and 3% inode usage. System available RAM was 9.2 GiB of 15.6 GiB.
Existing swap usage was about 2 GiB; short vmstat sampling showed no swap-out
and only minor swap-in. The trade06/trade07 log maintenance timer was active.
The Manager database occupied 13 GiB, pmdaemon logs 5.6 GiB, and PostgreSQL
2 GiB. Kline retention does not delete the position archive.

Kline startup succeeded: enabled status, no request error, and the first 7,200
candles downloaded. The theoretical index reached ready after processing about
3.24 million messages. During the first multi-symbol backfill, Manager RSS was
6,102,316 KiB (5.82 GiB), process swap zero, and system MemAvailable was
8,688,328 KiB (8.29 GiB). The Manager cgroup recorded zero OOM or OOM kills.
Filesystem usage remained 44%, 53 GiB available. The log-maintenance service's
latest run succeeded. The system's boot-cumulative OOM counter is historical;
no new Manager OOM was observed during this deployment.

### Account selector frontend follow-up

The operator reported overlapping account buttons after expanding to 11 visible
accounts. Frontend commit `f989389` moves the selector to its own full-width row,
wraps buttons and long labels, switches the shared navigation to its compact
layout below 1024 px, and lets narrow source rows shrink without page overflow.
The active account now has an explicit `aria-pressed` state.

Built with `npm run build`; lint passed with two pre-existing unused-variable
warnings in AuthGate/Field. Chromium rendering used the deployed account names
and mocked read-only API responses against the production bundle. Widths 1440,
1024, 768, 390 and 320 px all passed button/text containment, non-overlap,
page-width, selected URL, and source-scoped request checks. No local Vite server
was started. Desktop/mobile screenshots were inspected.

Published only static frontend artifacts as `20261008T073040Z`. The previous
frontend release remains available for rollback. Nginx served the index and both
assets with HTTP 200 and matching SHA-256 digests. Manager PID stayed 4050342;
runtime remains `1a3f88a`. No backend restart for this UI change.

After enabling Kline, first-load backfills and frontend verification, Manager
RSS/HWM was 6,838,460 KiB (6.52 GiB), swap zero, and MemAvailable was
7,744,532 KiB (7.39 GiB). Kline had downloaded more than 328,000 candles with
no reported request error. The latest one-day trade03 verification still had
pending minute backfills under the existing 120 weight/minute budget; a complete
all-symbol theoretical curve and repeat-with-zero-requests have not yet been
verified for that query. Factual curves remain available during loading.
