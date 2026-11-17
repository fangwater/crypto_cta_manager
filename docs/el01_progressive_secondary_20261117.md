# el01 progressive signal archive, reusable readers and cold discard — 2026-11-17

The operator requested the same treatment as jp-meta: new PB signals, bounded
online conversion/archival, reusable Exec history readers, and automatic discard
of unwanted expired cold signals every ten minutes. Only el01's host-level
Manager and seven Manager/helper binaries were replaced. Trading, Exec, Viz,
Config, Redis, Nginx, PostgreSQL, monitor, frontend and trading bindings were
preserved. No SQL DDL or whole-database network transfer occurred.

Release: `20261117T054820Z-progressive-secondary`.
The running Manager SHA-256 is
`b05991075d4610c3eb1f31a8141af58fcf12bb93dee25407fec5c91455ce0dcb`,
the same tested candidate deployed to jp-meta. It is a build of base `50b5ad4`
plus the tested uncommitted source-reader/native-lifetime fixes, not a clean
build of that commit. Source/binary hashes and validation results remain in
powerleader's `source-secondary-verification-20261117.json`. No Rust source was
changed or rebuilt during this rollout. See
[reader design](source_secondary_readers_20261117.md).

## Host-specific policy

The Manager DB identity is `7faca35f-ab4c-459e-abc8-5a4102458e3f`.
Cold staging is `/home/el01/crypto_cta_manager/cold-signals`.

- New signals write PB immediately. Original JSON is handled by the explicit
  progressive conversion policy; the oldest expired prefix moves into PB cold.
- Keep 33 days plus the existing execution guard in hot storage and preserve
  exact carried-inventory/target boundaries.
- Manager attempts one bounded migration per minute: at most 1,000 cold records
  / 64 MiB; retained hot conversion is bounded by 1,000 records / 8 MiB and a
  50 ms batch-building budget. These are not hard I/O latency bounds.
- Require 30 seconds quiet, I/O pressure at most 1%, no publication/archive
  backlog, idle background compaction and 10 GiB disk reserve. Manual range
  compaction is off; its independent cooldown remains 3,600 seconds.
- el01 shares all eight CPUs with trading. The busiest-core predeployment
  sample was 70.3%, so copying jp-meta's 35% threshold would starve migration.
  el01 uses 75%, retains the low-priority archive worker and the original
  Manager CPU affinity across 0–7. The per-minute record cap remains unchanged.
- `el01-cold-prune.timer` is enabled for every ten-minute wall-clock boundary.
  Its oneshot uses the same validated Python wrapper / Rust deletion helper as
  jp-meta, with source identity pinned to el01, CPU 7, Nice 19 and idle I/O.
  Limits remain 32 segments / 45 seconds, with a 90-second service timeout.
  Confirmed expired cold signals are deliberately discarded without backup.

Powerleader pull remains optional and manual; no pull process, timer or
continuous peer synchronization was enabled. The pruner never opens the hot DB
and never operates on Exec stores or PostgreSQL snapshots.

## Switch and temporary checkpoint

Old Manager PID 2471569 stopped beginning at 05:57:45.892 UTC. Its existing
15-second stop timeout expired and systemd killed it. The closed checkpoint
took 3.443 seconds; replacement PID 2751961 then started and bound the API at
05:58:24.276 UTC. Manager API interruption was approximately 38.4 seconds.
Exec continued existing targets throughout. Factual NAV became available again
near 06:05:20 after the first analytical rebuild; both factual and theoretical
indexes were ready by 06:05:37.

Both persistence queues were empty immediately before stopping. The final old
accepted signal was `rbf_small`, `updated_at_us=1794895065392829`, and this
exact value matches the replacement's durable original archive head. Every
enabled source's startup account-mode validation succeeded.

A short-lived checkpoint was captured with the old owner stopped and the POSIX
RocksDB lock held. It reused 119 independently precopied WALs (3,486,807,359
bytes), hardlinked 35,300 immutable SST files and independently copied another
76,783,613 mutable bytes. All 35,426 captured data files passed catalog/size and
mutable-file hash verification; native first/last JSON readback passed.
WALs and mutable metadata were never hardlinked.

After financial, process, configuration and scheduled-deletion verification,
the exact temporary checkpoint directory was removed to release obsolete SST
pins. No full DB was returned over the network. Free space increased from
32.60 to 36.63 GiB during that cleanup. The old binaries/configuration, receipts
and compatible PB fallback remain; current data was not replaced.

## Verification

All 95 protected process identities, 62 configuration hashes and 11 key SQL
tables matched before/after, except the explicitly enabled Manager archive
configuration. Funding switches, frontend and original Manager affinity were
unchanged. These checks also passed after automatic cold deletion.

Two fixed six-hour BTCUSDT financial reports retained exact hashes:

| Source | Fills | SHA-256 |
| --- | ---: | --- |
| trade06 | 614 | `53a9255950020b262ff3be171ceb90f58269500f85add230ef8c0ecd171268c8` |
| trade07 | 749 | `b008824e872496c8652e23734649b8ac7f8539024e5bbbda08b4aa1dc12e7403` |

Both contained 26 points, covering `startMs=1794865800000` through
`endMs=1794887400000`. Temporary diagnostic sessions were deleted after each
check. Available labels/history bounds were excluded from equality because
normal trading can add metadata; the historical financial reports were identical.

Post-warm dashboard refreshes were approximately 2.5–3.5 seconds, with no new
source secondary opened after 06:06:20 during observation. Six successive
30-second process CPU samples were 7.07%, 16.13%, 7.20%, 16.60%, 6.70% and 17.00%
of one core. File descriptors stayed near 1,891, below the unchanged 65,536
service limit. Available memory remained approximately 6 GiB. These are short
observations covering refresh/idle phases, not guarantees for arbitrary query
load. Archive and PG mirror queues stayed empty, with no cold worker errors.

The first automatic timer trigger at 06:10 UTC / 14:10 Shanghai successfully
discarded four sealed expired segments / 235 records. All four directories were
confirmed absent, and the final carried-inventory boundary remained present.
These earliest historical days contain few signals, so a day-bounded batch can
have fewer than the 1,000-record maximum. Migration continued afterward.

The Python wrapper's six tests (including two real native RocksDB integrations)
had already passed on the same artifact. The el01 service/timer passed local and
remote `systemd-analyze --user verify`, then the actual scheduled trigger above.

## Recovery and operations

The old JSON-only owner cannot read newly accepted PB events. If the source
reader needs rollback, use the prepared compatible `cta_web.fallback` (SHA-256
`e56ea5b360d5c076086472a3a5c5229e9b82d730136d2ef33d1197cc6fc04047`) while
preserving the current DB and archive configuration. Do not restore the old
checkpoint over the newer signal suffix.

```bash
systemctl --user list-timers el01-cold-prune.timer
journalctl --user -u el01-cold-prune.service --since today
```

To stop only scheduled discard:

```bash
systemctl --user disable --now el01-cold-prune.timer
```

Local evidence: `/home/fanghaizhou/artifacts/el01_20261117T054820Z-progressive-secondary`.
Remote receipts/fallback: `/home/el01/crypto_cta_manager/releases/20261117T054820Z-progressive-secondary`.
Retained binary/config backup: `/home/el01/crypto_cta_manager/backups/20261117T054820Z-progressive-secondary`.
