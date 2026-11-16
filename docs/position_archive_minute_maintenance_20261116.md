# Minute-batched Manager maintenance, 2026-11-16

The later [progressive conversion](position_archive_progressive_conversion.md)
adds optional in-place hot JSON conversion and PB new writes. Automatic manual
range compaction now defaults off; explicit opt-in has an independent persisted
cooldown (one hour by default, at least 30 minutes). It also avoids rebuilding
the theoretical target index for every minute-sized retention movement.

The operator changed the Manager cadence from hourly to once per minute and
confirmed that powerleader pull is explicitly operator-controlled. Manager does
only local hot-to-cold migration and reclamation of already-archived hot space.
No deployment, Manager restart, live pruning or automatic pull has been enabled.

## Defaults and ownership

```toml
[archive.cold]
root = "/home/el01/crypto_cta_manager/cold-signals"
interval_secs = 60
quiet_secs = 30
max_cpu_percent = 35.0
max_io_pressure_percent = 1.0
min_free_bytes = 10737418240

[archive.cold.limits]
max_records = 1000
max_bytes = 67108864

[archive.cold.reclaim]
max_input_bytes = 16777216
max_files = 4
```

One bounded cold WriteBatch per eligible attempt, oldest first, never spanning
Shanghai days. The byte budget remains because signals can vary greatly in size.
The persisted last-attempt marker enforces the minute interval across restarts;
load sampling/quiet time can defer work. Jobs never overlap. Preserve 33 days
plus the execution guard in hot storage. Existing JSON hot writes stay JSON;
only migrated cold records become self-contained PB.

At most one hot signal column family/range is reclaimed after an attempt. Prefer
whole expired non-L0 SST deletion through RocksDB's API, without rewriting data.
Require a bounded closed overlap group strictly below the verified floor, to
avoid uncovering older versions hidden by range tombstones. PB CFs rotate;
JSON operates on `position_updates` only. Active readers/background compaction
defer work, and signal readers explicitly enforce the persisted retention floor.

Small range compaction budgets full transitive SST overlaps at every level plus
the current memtable footprint (which may need flushing), at most 16 MiB and
four observed files by default. It uses a nonexclusive operation without a level
move. Larger/shared work is skipped for ordinary background compaction. This is
a conservative preflight, not a hard runtime/CPU/IO limit: SST boundaries and
concurrent background changes can expand actual work. RocksDB's reported
background errors are checked before/after the request. Never compact default,
Kline, NAV or Exec CFs as part of this worker.

Before making copies, both filesystems need the 10 GiB reserve plus four times
the cold WriteBatch cap. Small compaction needs its reserve plus four times its
input cap; metadata-only obsolete-file deletion can still free space. Low or
unknown capacity retains unsealed originals and reports an error. This protects
maintenance headroom, not all unlimited future writes if cold is never pulled.
Source package generation is a separate operator operation.

The health `cold_archive.hot_reclaim` report gives the action, column family,
observed file/input bytes, duration, defer reason and error. Actual physical
space release can lag because RocksDB still owns/pins files.

## Explicit local pull

The existing public SCP export/restore/verify/delete protocol is unchanged.
Manager never calls it. An explicit manual invocation bypasses the optional
daemon's cooldown; a finite batch helps drain minute-sized segments:

```bash
cta_archive_pull --config /absolute/private/powerleader.toml --host el01 --run --reclaim-source --max-segments 1440
```

Default is one segment. `--max-segments` is bounded to 4096 and conflicts with
`--watch`; it runs once and exits. Every segment is independently durable and
verified before its own source removal. An error stops the batch with prior
receipts preserved. Omit `--reclaim-source` for backup only. No timer or pull
process is registered. The optional daemon mode retains its existing explicit
hourly-or-slower configuration; it is not selected by Manager maintenance.

## Verification

Focused tests cover a persisted 59/60-second restart boundary, count-limited
JSON/PB movement, low-space refusal without sealing/deleting originals, overlap
and memtable budget rejection, actual small JSON compaction, actual whole-SST
reclamation/restart, and reader exclusion of obsolete physical rows. Protected
Kline/current-target records remain exact. Existing full tests also cover priced
monthly curves for all four execution models, checkpoints and cold recovery.
Full validation results and release hashes are recorded in the local release
manifest and `rust-cold-minute-tests-20261116.log` on powerleader NVMe.

Final validation: 588 library and 18 binary tests passed; the existing 17
operator/benchmark and 19 isolated PostgreSQL cases remain ignored. The manual
Python workflow's 12 tests, deployment shell syntax, formatting and checks
passed. All four archive/Manager release binaries built successfully.
