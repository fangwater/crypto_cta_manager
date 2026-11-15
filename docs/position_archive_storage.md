# Compact Manager position archive

The current implementation stores position signals as Protobuf, separates target
inputs from factual observations, interns repeated strings, and shares/replaces
snapshot contents without changing their numerical precision. JSON remains the
HTTP publish/query contract. This replaces the sole live archive format; it is
not an additional format selected per row. Legacy JSON is handled exclusively by
explicit offline migration/import commands. No live host has been migrated as
part of developing this implementation.

## Layout and financial semantics

All three column families belong to Manager's existing host-global RocksDB:

| Column family | Contents | Key |
| --- | --- | --- |
| `position_updates` | Immutable target, frozen bindings/shares/fees and projections | Original `(received_at_us, seq)` |
| `position_observations` | Source, observation timestamp, readiness and snapshot reference | The same original message key |
| `position_snapshots` | Day-local content-addressed position bodies and compression heads | Day block, record kind, SHA-256 |

`src/position_archive/storage.proto` defines the single binary contract. Every
target record carries its own string dictionary; there is no mutable global
symbol-number registry to synchronize or restore. Factual quantity/USDT arrays
use packed doubles, plus a separate bitmap for optional USDT values. Null,
numeric zero, signed zero and the explicit target clear instruction remain
distinct. All existing known message fields, identities, ordering, observed
times, frozen fees, shares and contract projections are retained.

The snapshot content hash includes its source/strategy namespace and complete
ordered position payload. Observation time/readiness stay in each observation,
so identical positions can share a body while preserving separate observations.
Both quantity and USDT value participate in equality; equal quantities with
different observed USDT amounts do not get merged.

Snapshot parents are confined to one Shanghai calendar day block and one namespace. An unchanged
body is reused. Otherwise the writer compares a complete baseline with a
replacement of changed slots and stores the smaller representation. A
replacement contains new absolute doubles, never arithmetic quantity deltas.
A symbol-list/order change starts a complete baseline. There are at most 31
parent links before another full baseline, so random lookup and recovery remain
bounded. Two disposable in-memory caches each cap entries at 128 and accounted
payload size at 16 MiB.

New targets preserve the previous archive JSON parsing boundary once before
binary encoding; global `serde_json` parsing features are unchanged. Existing
JSON records are parsed once by the importer and then retain their exact typed
floating-point bits. Factual enrichment updates only observations/snapshots and
does not rewrite frozen targets or invalidate the theoretical target index.

Theory scans only `position_updates`. It no longer reads/decompresses factual
payloads while building the target index. Existing ordered incremental scans,
recent overlap, late-insert invalidation, carry, fees, price marks, FIFO, curve
caches and query policy remain in place. Expected improvements are archive I/O,
target-index initialization and background persistence, with no change to the
financial result. A cached interactive curve may see little improvement.

Current target recovery still uses the existing default-CF entry, atomically
written in the same synchronized WAL batch as its target event. Publication
continues to use the existing bounded asynchronous writer; disk work never joins
the normal target-publication critical path. No PostgreSQL schema, trading
configuration, Exec store or exchange API is changed.

## Explicit migration and recovery

Build the matching owner and tool together:

```bash
cargo build --release --bin cta_web --bin cta_position_archive
```

An existing JSON store is refused at normal startup with an explicit migration
instruction. It is not converted at startup. The offline tool must own the
exclusive RocksDB lock; Manager must be stopped before operating on its live
path. Never remove `LOCK`, stop Exec, or run this against an Exec store.
`check-storage --db PATH` is read-only; the standard deployment script runs this
gate with the uploaded archive client before switching binaries or restarting
Manager. A legacy/incomplete store aborts deployment with the running owner intact.

For a selected, authorized database, supply absolute paths to the existing DB
and a new recoverable backup location outside it:

```bash
cta_position_archive migrate-json \
  --db /absolute/manager/db \
  --backup /absolute/backup/original-manager-db
```

For the operator's powerleader NVMe backup policy, first use `backup-json --db
PATH --backup NEW_CHECKPOINT` while Manager is stopped. This only prepares and
verifies the complete original checkpoint. Copy that directory, preserving
`IDENTITY` and `POSITION_JSON_BACKUP.json`, to powerleader's NVMe archive root.
Run `verify-json-backup --db NVME_CHECKPOINT` there; it independently checks every
CF against the saved proof. Only then run `migrate-json` with the same unchanged
source and source-side checkpoint. Both steps are manual; no ongoing peer or
scheduled synchronization is installed. Keep the source checkpoint until the
NVMe copy and post-migration state are verified. Source checkpoint removal must
be an explicit host/path-specific cleanup, since its SST hard links otherwise
retain disk space even after compaction.

Before replacing any signal, the tool flushes WAL, checkpoints the complete DB,
preserves `IDENTITY`, fsyncs the backup, and independently opens it to verify
logical hashes of every original column family. `POSITION_JSON_BACKUP.json`
records that proof. An existing verified backup may be reused only for the same
identity and unchanged original database. A checkpoint may share SST files on
the same filesystem; retain an independently copied and verified backup on
powerleader NVMe before remote space reclamation. Backup location and disk
capacity are part of the host-specific rollout plan.

Each converted record and its resume cursor commit in one WAL batch, with
bounded memory and periodic durable WAL flushes. An interruption leaves a
migration marker; normal startup refuses that state. Repeat exactly the same
command with the same backup to resume. No automatic continuation or timer is
installed.

Before marking the store usable, a fresh storage reader checks every record
against the original JSON backup using exact typed serialization, including
complete factual observations. Record counts and all unrelated CFs/default-CF
data, including recoverable current targets, must match. The final format marker
and removal of the migration marker share one synchronized batch.
`POSITION_STORAGE_MIGRATION.json` records count, typed-history SHA-256 and the
unrelated-data comparison. `--compact` explicitly flushes/compacts the three
signal CFs only after those checks; no other CF is compacted by this command.

Rollback requires the complete original backup and the old owner/tool. A JSON
reader cannot open the converted signal records. Restore into a separate path,
verify the original identity and logical data, then switch the Manager DB path
while Manager is stopped. Keep the converted DB too until the replacement is
verified. Neither rollback nor migration publishes targets.
Direct restoration of the original checkpoint is safe before reopening target
admission. After accepting new binary-format events, preserve that newer DB and
plan/verify recovery of its new suffix before returning to an older checkpoint;
the original checkpoint alone cannot contain those later accepted signals.

Before switching binaries, preserve the currently running owner and the installed
archive/analysis tools on powerleader NVMe. Record their hashes, sizes, modes and
which candidate helpers were previously absent; verify the saved owner matches
the running executable. Restore that original set on rollback. Remove a helper
introduced only by this candidate only when its current hash still matches the
candidate manifest, so a PB-only helper is not left reading the restored JSON DB.
Preserve the existing service unit, drop-ins, host TOML and actual process CPU
affinity during the format rollout.

## Manual date-based backup/reclamation

The existing manual date script remains the operator workflow; no automatic
retention schedule is introduced. Export reconstructs each complete message as
canonical, self-contained Protobuf. A backup has no external dictionary or
snapshot-parent dependency. The gzip stream retains those exact frame bytes.
Restore checks the frame receipt, reconstructs an isolated split store, and
reads/re-encodes every full message before accepting its receipt.

Only after that verification may the owner atomically save the boundary and
delete the target/observation prefix and wholly obsolete snapshot day blocks.
No retained snapshot can point into an older day block. Manual `--before`
dates align exactly with those blocks. A lower-level boundary inside a block
conservatively retains that entire block until a later manual run.
Compaction includes all three signal CFs. Klines, current targets, PostgreSQL,
Exec history, funding and factual FIFO are unaffected.

Restore older JSON gzip files explicitly, completing their old receipt chain:

```bash
cta_position_archive verify-json --file OLD.signals.gz --restore /new/offline/db
cta_position_archive migrate-json --db /new/offline/db --backup /new/offline/original-json
```

Later binary chunks use the normal `verify` command against that migrated
offline destination; its saved old receipts preserve chain continuity. Normal
`verify` never guesses JSON from bytes, and `verify-json` refuses a binary or
partially migrated destination. Original legacy gzip files/checkpoints remain
unchanged.

Read-only `inspect` sums the three CFs for total/eligible space and reports their
individual SST sizes. `nav_cached_marks --target-source` reads binary targets.
Diagnostic sampling defaults to the current format; `--legacy-json` explicitly
selects an old source. `rehearsal-prepare --legacy-json` restores and migrates a
sample in a new isolated directory, preserving a complete original JSON copy.

## Validation

Focused storage tests cover complete-field/bit preservation, malformed
dictionaries/bitmaps, bounded chains, identical-body sharing, observation-time
preservation, namespace/day isolation, repeated namespaces within one atomic
batch, target-only scans with missing snapshots, backup/restore/prune/cold
restart, original backup checks, exclusive ownership, interrupted migration
resumption, and continuing a legacy receipt chain after explicit conversion.

The financial regression independently parses the original JSON into the
theoretical model, then compares migrated cold-reader results. It covers two
accounts/strategies, frozen shares/quote fees, projections, repeated timestamps,
clears and symbol removal, and compares full minute-priced curves plus
account/strategy acquisition inputs for 5-minute TWAP, 3-minute TWAP, 3-minute
VWAP and instant taker execution. Full-crate tests also exercise the existing
retained-window and current-target/publication behavior.

`rehearsal-codec` compares the original JSON, the earlier inline PB experiment,
and the current complete split layout with identical keys/LZ4 settings. Its
reported split size includes targets, observations, blob bodies and compression
heads. It validates a fresh-reader full-history hash and theoretical-input
equality. Its five-pass median target scan/decode/hash time is a sample CPU/I/O
measurement, not an end-to-end production NAV latency claim.

## Real-record measurements, 2026-11-15

With identical keys and normal RocksDB LZ4 settings, the complete compact layout
gave these results. Sizes include all three signal CFs and compression heads;
MB uses decimal units. Every dataset passed full typed cold readback and exact
theoretical-input equality.

| Dataset | Records | Original JSON SST | Earlier inline PB SST | Compact SST total | Reduction versus JSON |
| --- | ---: | ---: | ---: | ---: | ---: |
| Previous el01 recent sample | 800 | 3,379,346 | 2,074,704 | 1,276,566 | 62.2% |
| Previous el01 multi-day sample | 1,520 | 5,164,592 | 3,129,021 | 1,991,046 | 61.4% |
| Fresh el01 contiguous suffix | 2,000 | 6,922,151 | 4,059,446 | 2,115,036 | 69.4% |
| Fresh jp-meta contiguous suffix | 2,000 | 6,176,967 | 3,884,234 | 2,135,438 | 65.4% |

Five-pass median scan/decode/hash times were 62,428 → 19,453 microseconds for
the fresh el01 sample and 62,558 → 16,832 microseconds for jp-meta: roughly
3.2–3.7 times faster for this target-input scan. These are small samples with
warm repeated reads; full production migration, cold index initialization and
interactive priced NAV latency were not measured. Do not multiply either the
space ratio or the speedup directly into a production promise.

The fresh samples were taken through read-only secondaries. The probe opens
only signal-related CFs and pins their source files; it excludes the large
unrelated Kline CF. An initial probe hit the diagnostic process's file-descriptor
limit and was cleaned up. The successful retry raised only its own process soft
limit and did not change host/service limits. Both hosts' protected process
identities, DB identity, Manager config hash and canonical archive-client hash
matched before/after. All remote probe binaries/secondary directories were
removed. No live signal was migrated, sealed, deleted or compacted.

Private powerleader evidence root:
`/mnt/nvme-raid0-28t/fanghaizhou/crypto_cta_manager_archives/compact-20261115155402Z/`.
It contains original gzip samples, codec reports, dataset spans, source-state
proofs and cleanup records. Temporary original-format probe failures remain
separate from the successful `live-tail/` evidence.

The same root's `manual-real/` rehearsal restored/migrated the 1,520 real JSON
records, exercised backup-only checkpoint preparation and independent copied
checkpoint verification, and ran the actual manual Python script through an
isolated owner socket. The October 1 prefix backed up and reclaimed 720 records
(1,597,864 self-contained PB frame bytes). All 544 sampled records in the recent
month, 25 source/binding entries and 1,436 cumulative source/symbol entries kept
identical full-message and opening/closing inventory hashes after reclamation
and cold restart. Repeating reclamation was idempotent. The isolated owner and
its socket were removed. This input/inventory proof is separate from the priced
curve regression and does not claim a full-production priced NAV comparison.

Final validation: 563 library tests and 18 binary tests passed, with 17 existing
library/operator benchmarks and 19 isolated-PostgreSQL tests ignored; the 12
manual-workflow Python tests passed. Formatting, all-target compilation, shell
syntax and the complete release build passed. No frontend or SQL schema changed.

Pre-migration rollback preparation also saved five installed binaries from each
host under the private evidence root's `rollback-binaries/`. Both running Manager
executables matched their saved copies, all copied hashes matched, and source
process/configuration identities stayed unchanged. `nav_cached_marks` was absent
on both hosts and is recorded as a newly introduced helper for rollback handling.
Each saved original archive client successfully inspected the preserved original
JSON checkpoint locally. These steps did not migrate a live DB or restart a service.
