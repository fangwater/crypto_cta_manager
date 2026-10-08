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
