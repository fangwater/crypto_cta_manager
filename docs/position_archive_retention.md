# Manual position signal backup and RocksDB reclamation

The operator's latest automatic-storage direction is recorded in
[the hourly local-cold and powerleader pull design](position_archive_cold_design.md).
The Rust cold worker and `cta_archive_pull` are implemented separately from the
manual workflow below; no live hourly timer has been enabled.

For the immediate accumulation problem on the currently deployed JSON stores,
see [the prepared JSON-only online reclamation plan](position_archive_accumulation_20261116.md).
That package preserves JSON and does not require a full PB migration. The normal
checkout and deployment gate below describe the newer PB owner; do not deploy
that owner onto a live JSON store as part of the accumulation cleanup.

Operator policy, 2026-11-15: use an explicit host and date for each manual run.
The manual workflow has no automatic retention job, host iteration or scheduled
compaction. The separate opt-in cold worker is described in the linked document.
The earlier hourly service/timer templates have been removed. Powerleader's
previously staged timer remains disabled; no live signal pruning has occurred.

The current compact implementation covers Manager's `position_updates`,
`position_observations` and `position_snapshots` signal column families. See
[`position_archive_storage.md`](position_archive_storage.md) for its explicit
JSON migration and backup contract. Existing hosts still require a separately
authorized coordinated rollout; no live migration was run during development.
Current targets,
Exec orders/fills, PostgreSQL snapshots, funding, FIFO and Kline maintenance
are independent. Factual 60D/90D/ALL NAV history is unaffected. Older raw signal
browsing and signal-dependent analysis require the offline archive after pruning.

## Ownership and permission

`cta_web` owns the live Manager RocksDB write handle and its process lock.
An independent script cannot open that same database for writing while Manager
is running, even when it uses the same OS account. It must not remove the lock
or SST/WAL files. Read-only planning can open the store independently.

The script uses SSH as the configured Manager OS user and asks that Manager
process to export, commit verified deletion and compact through a private Unix
socket. The parent directory must be Manager-owned and private (0700); the
socket is 0600. There is no public HTTP maintenance endpoint, browser-token
permission or exchange credential in this workflow. `cta_monitor` stays read-only.
The external script writes only the independent backup on powerleader NVMe.

Compaction alone does not delete records by date. The required sequence is:

1. Preview the exact prefix and approximate size, without any mutation.
2. Stream and restore/verify a complete backup; retain all live original records.
3. Explicitly request reclamation: Manager verifies the same prefix again,
   atomically saves its boundary checkpoint and deletes that prefix and obsolete
   snapshot blocks, then compacts only the three already reclaimed signal CF ranges.

## Exact date semantics

`--before YYYY-MM-DD` means strictly before that date at 00:00 **UTC+08:00**.
For example, `--before 2026-10-01` excludes all records on/after
2026-10-01 00:00 Beijing time (2026-09-30 16:00 UTC). All same-timestamp sequence
numbers at the boundary are retained. Retrying never extends the date to today.

Manager preserves the 30-day analysis window plus three additional days. It
rejects a date that would remove this retained 33-day history. The maximum safe
boundary is UTC midnight at least 33 days ago minus ten minutes for execution
schedules and signal matching. An older manually chosen date
retains more history. Without another manual run, retention grows rather than
advancing automatically.

The boundary checkpoint preserves each source/binding's last normalized target
and exact ordered cumulative source/symbol quantities. It uses MessagePack
independently of original JSON numeric parsing. Startup and theoretical replay
seed from this checkpoint, then read the retained suffix. Immutable allocation
identity, source/venue isolation, current-target recovery and factual order
history remain unchanged.

## Manual commands on powerleader

Copy `deploy/position_archive/powerleader.example.toml` to
`~/.config/crypto-cta-manager/position-archive.toml`, mode 0600. It is a host
catalog; every command requires exactly one explicit `--host`. The previous
`enabled` host switches and `--apply`/`--max-chunks` workflow are no longer used.
Backup root:
`/mnt/nvme-raid0-28t/fanghaizhou/crypto_cta_manager_archives`.

Preview only; this also works before the Manager socket is enabled:

```bash
python3 scripts/pull_position_archive.py \
  --config ~/.config/crypto-cta-manager/position-archive.toml \
  --host el01 --before 2026-10-01
```

Backup and verify the entire selected prefix, with **no live deletion**:

```bash
python3 scripts/pull_position_archive.py \
  --config ~/.config/crypto-cta-manager/position-archive.toml \
  --host el01 --before 2026-10-01 --backup
```

Reverify that exact saved backup, request one atomic prune, then one compaction:

```bash
python3 scripts/pull_position_archive.py \
  --config ~/.config/crypto-cta-manager/position-archive.toml \
  --host el01 --before 2026-10-01 --reclaim
```

Use `--host jp-meta` explicitly for the other host. Dates above are examples,
not an instruction to run reclamation. No selected live range has been applied.

The default preview creates no backup directories and performs no seal, export,
delete or compaction. It reports identity, original/current range, requested
boundary, maximum safe boundary and approximate eligible SST bytes. Estimates
exclude unflushed data and do not promise immediate space reclamation.

## Backup, verification and retry

A backup seals the selected old prefix: late factual enrichment skips it and
historical insertions into it fail closed. Normal current publications continue.
The 12-byte signal keys remain unchanged. The current owner exports complete,
self-contained canonical PB messages assembled from targets and observations;
snapshot references never escape the source. Original JSON survives in the
explicit migration backup and earlier gzip archives. The gzip backup preserves
its exact exported frame bytes, without cross-message dependencies.
The entire date prefix is streamed as one gzip file without making a duplicate
export on the live host or loading the whole range into memory. Header/frame
sizes remain bounded. A failed stream is retried from the start of that date
prefix; automatic chunk-level resumption has been removed.

Powerleader fsyncs the compressed file, restores it to a dedicated local RocksDB,
and reads the entire range back. Identity, receipt chain, SHA-256, key ordering,
message timestamps, counts, byte counts, EOF and gzip CRC are checked. It then
atomically renames/fsyncs the gzip, receipt and date manifest before reporting
backup completion. Both compressed originals and the verified restored store
reside on powerleader; they share the same NVMe RAID0 failure domain and are not
independent hardware redundancy.

Files below the host's private backup directory:

- `identity.json`: pinned live database identity.
- `chunks/<sha256>.signals.gz` and `.json`: original stream and verified receipt.
- `restored/`: verified offline RocksDB with original records and receipt chain.
- `before-YYYY-MM-DD.json`: immutable backup/date/receipt manifest.
- `before-YYYY-MM-DD.reclaimed.json`: completion record after successful compact.

`--reclaim` refuses a missing, corrupt, wrong-date or foreign-identity backup.
It revalidates the saved stream/readback before contacting the owner to delete.
Manager independently rescans and verifies the sealed range before one synced
WAL batch saves the checkpoint/receipt and deletes the range. There is exactly
one commit for the entire chosen prefix, so interactive queries do not trigger
one full theoretical-index rebuild per old 64 MiB chunk. The next cache refresh
can still rebuild the retained suffix once.

A lost commit reply or an interrupted/failed compaction can be retried with the
same `--reclaim` command. An already reclaimed prefix is not deleted again;
compaction is still requested. Manager flushes the WAL durably before reclaiming
SST space, including a retry after a previously visible commit reported a sync
error. It rejects compaction beyond its verified reclaimed boundary. Backup manifests remain available after success. A single
local flock serializes manual backup/reclaim jobs.

If powerleader disconnects before deletion, source originals remain. If it
fails after the verified commit, the durable backup remains the recovery source
and the operator retries compaction. No periodic peer connection is needed.

## One-time Manager deployment prerequisite

Build locally in the maintained checkout:

```bash
cargo build --release --bin cta_web --bin cta_position_archive
```

Use the matching `cta_web` and client from this build; there is one current
maintenance contract. The old client without date arguments must be replaced
with the owner code. Installing the read-only client alone enables planning,
not backup/reclamation.

For a selected live host, add this independent section to its existing TOML,
substituting the absolute Manager root, and restart only Manager once:

```toml
[archive]
socket = "/absolute/manager/root/archive-control/position.sock"
```

The normal deploy script installs `cta_position_archive`. Preserve the existing
host configuration, account modes, trading bindings, Exec RocksDB and trading
process identities. Backup/reclaim subsequently run through the existing owner;
they do not stop Manager or trading. The endpoint itself performs no automatic
maintenance. It is currently **not enabled on either live host**.

Install the powerleader tools outside Cargo's disposable build cache:

```bash
install -d -m 0700 /mnt/nvme-raid0-28t/fanghaizhou/crypto_cta_manager_archives/bin
install -m 0700 /mnt/nvme-raid0-28t/fanghaizhou/cargo-target/crypto_cta_manager/release/cta_position_archive /mnt/nvme-raid0-28t/fanghaizhou/crypto_cta_manager_archives/bin/cta_position_archive
install -m 0700 scripts/pull_position_archive.py /mnt/nvme-raid0-28t/fanghaizhou/crypto_cta_manager_archives/bin/pull_position_archive.py
```

The owner thread lowers its CPU niceness and performs no transfer on the normal
publication path. Manual compression, verification and RocksDB compaction still
share live CPU/storage resources. Choose a quiet maintenance window and use the
preview to choose a manageable date range; there is no hard remote I/O throttle.
No online latency benchmark or financial comparison after a real prune has yet
been performed. Do not describe offline test results as zero production impact.

## Offline recovery

Create a new, absent restore directory and verify saved gzip files in receipt
chain order:

```bash
cta_position_archive verify \
  --file /backup/el01/chunks/<sha256>.signals.gz \
  --restore /new/offline/archive-db
```

The verifier rejects foreign stores, conflicting bytes, missing previous
receipts and incomplete streams. `archive_boundary:*` and `archive_receipt:*`
retain the boundary envelopes and chain. Recovery does not publish targets,
write PostgreSQL or write any Exec store. This recovers signal archives; it is
not a complete PostgreSQL/Exec/Manager backup.

The live health API exposes `position_archive_retained_from_us`; raw-history
responses expose `x-cta-archive-retained-from-us`, and old cursors return 410.
Rollback after pruning must retain checkpoint-aware code. Never erase checkpoint
metadata to roll back.

## Review evidence and current state, 2026-11-15

Read-only inspection before any live pruning found:

| Host | Estimated messages | Eligible SST bytes (approximate) | Filesystem available |
| --- | ---: | ---: | ---: |
| el01 | 6,957,076 | 13,279,405,504 (~12.37 GiB) | 30 GiB |
| jp-meta | 7,840,852 | 12,492,088,962 (~11.63 GiB) | 511 GiB |

This estimate used the then-current maximum safe boundary,
2026-10-15 23:50 UTC, not the example `2026-10-01` date above. Powerleader had
approximately 9.3 TiB available. No live seal, backup, prune or compact was run.

Review found per-chunk index rebuilding and a pending-commit compaction omission
in the earlier automatic design. One full-prefix backup/commit and unconditional
retriable compaction now address those paths. Scheduled failure/lag alerts are
not part of this manual workflow; commands report failures with nonzero exits
and preserve recovery manifests. Operators must still watch live free space.


## Manual tooling validation, 2026-11-15

`cargo fmt --check`, `cargo check --all-targets`, full `cargo test` and release
builds passed. The full crate reported 552 passing tests, 17 ignored library
tests and 19 ignored operator/integration checks. Twelve focused retention tests
include a real Unix-socket export/verify/status/commit/compact exchange, live
write-lock exclusion with read-only planning, exact date-boundary preservation,
current-target/head recovery, CF isolation and warm/cold financial equivalence.
Eleven Python tests cover preview-only defaults, separate whole-prefix backup
and reclamation, failed transfers/verifiers, source identity/range changes,
lost commit replies and retrying compaction after deletion.

The manual script and verifier were installed in powerleader's private backup
`bin/` directory, and its host catalog gained explicit absolute Manager DB paths.
Both obsolete local systemd units were disabled and removed only after their
contents matched the earlier repository templates exactly. No scheduler remains
for this feature. Previous tools/config are preserved privately under
`manual-tooling-20261115/` on the selected NVMe backup filesystem.

The date-aware read-only client was checksum-verified and atomically installed
on el01 and jp-meta; exact `.next-20261115-manual` staging paths were removed by
rename. A default preview for `--before 2026-10-01` succeeded on each host:

| Host | Eligible SST estimate for this example date | Preserved Manager PID |
| --- | ---: | ---: |
| el01 | 7,552,147,548 bytes | 2471569 |
| jp-meta | 5,199,402,942 bytes | 438574 |

Both source floors remain `(1, 0)` and both have no committed archive receipt.
Manager process start identities and host TOML hashes were preserved. These are
read-only previews, not selected cleanup ranges. No live owner endpoint was
enabled and no live backup/seal, prefix deletion or compaction was performed.
Detailed local evidence is in `artifacts/manual_position_archive_20261115/`.


## Real-record isolated trial, 2026-11-15

The operator requested a trial without yet selecting an exact live reclamation
scope. The test used the running el01 Manager's real signal records through a
read-only, SST-pinning secondary. Sampling was bounded to day/edge records
(128 MiB cap), not a copy of the complete production history. The temporary
source secondary and staged diagnostic binary were removed after verification.
Source Manager PID 2471569/start identity, its TOML hash and its installed archive
client were unchanged. Source floor/sealed remained `(1, 0)` with no receipt.
No live endpoint was enabled and no live signal was sealed, deleted or compacted.

A new powerleader rehearsal root restored the sample and created a separate
writable checkpoint. Both copies have explicit identity-bound rehearsal markers.
`rehearsal-serve` refuses an unmarked database, preventing accidental use of the
production path. It uses the same owner socket/export/commit/compact implementation.
The manual Python script also supports `transport = "local"` for a same-machine
owner; existing remote hosts retain their SSH transport. The test exercised the
actual script, CLI, socket, gzip, restore and RocksDB operations, without mocked
backup or deletion responses.

Trial scope and results:

- Real sample: 1,520 records, 11,604,595 raw frame bytes, 2,754,928 gzip bytes.
- Example cutoff: 2026-10-01 00:00 UTC+08:00, on the isolated copy only.
- Backed up and removed from that copy: 720 records / 4,259,861 raw frame bytes.
- Independent compressed backup: 880,210 bytes; restore into a second new DB
  returned the exact same count, range, chain and SHA-256 receipt.
- Retained 30-day sample: 544 signals, 25 source/binding entries and 1,436
  cumulative source/symbol entries. Original signal-byte hashes and opening/end
  reconstructed inventory hashes matched exactly before and after cold restart.
- Working-copy live signal SST size: 5,164,565 → 3,378,925 bytes. This is an
  isolated CF measurement, not reclaimed production space; the reference copy
  deliberately preserves the original test data on NVMe.
- Reclamation without a saved backup was rejected. A second writable opener was
  rejected while the isolated owner held its DB lock. Backup alone retained all
  original rows; repeating `--reclaim` preserved the same floor and receipt.

This validates the real-record sample's reconstruction inputs and the full
maintenance flow. It does **not** assert equality of all production NAV curves,
fees, slippage or full-history account holdings; no complete priced production
query was replayed. Synthetic retained-month priced-curve tests remain separate.

Evidence root (private, powerleader NVMe):
`/mnt/nvme-raid0-28t/fanghaizhou/crypto_cta_manager_archives/trial-1115-133747/`.
`result.json`, before/after proofs, independent recovery receipt and source
before/after/cleanup records preserve the checks. The isolated owner process was
stopped. Live Manager and trading processes were not restarted. Thirteen focused
Rust retention tests, twelve Python tests, all-target compilation and the release
archive-client build passed for the diagnostic additions.

## JSON versus Protobuf size experiment (2026-11-15)

`cta_position_archive rehearsal-codec --file SAMPLE --root NEW_ABSOLUTE_DIR`
compares encodings using a previously exported gzip sample. It creates three
isolated Manager databases with identical signal keys and the normal LZ4 options:
original JSON bytes, JSON reserialized through the current typed parser, and an
experimental typed Protobuf encoding. It flushes and compacts each signal CF and
reports actual live SST bytes. The output directory must be new. This command
does not open a live database or change the production archive encoder/decoder.

The experiment used the real el01 sample from the rehearsal above. A second
input retained the 800 sampled records since 2026-10-01 00:00 UTC+08:00. This is
a diagnostic subset, not a production backup or deletion receipt. Sizes below
are exact bytes; MB comparisons use decimal units.

| Sample | Encoding | Uncompressed value bytes | LZ4 signal SST bytes |
| --- | --- | ---: | ---: |
| Recent 800 records | Original JSON | 7,328,734 | 3,379,346 |
| Recent 800 records | Reserialized JSON | 7,323,815 | 3,376,170 |
| Recent 800 records | Experimental Protobuf | 3,402,124 | 2,074,704 |
| All 1,520 records | Original JSON | 11,574,195 | 5,164,592 |
| All 1,520 records | Reserialized JSON | 11,573,203 | 5,162,824 |
| All 1,520 records | Experimental Protobuf | 5,549,146 | 3,129,021 |

For the recent sample, Protobuf reduces raw values by 53.6% and actual LZ4 SST
bytes by 38.6% versus original JSON. The full sample's SST reduction is 39.4%.
Reserializing JSON alone produces negligible savings, so the observed difference
comes from encoding rather than dropping known fields. These are small el01
samples, not measurements of the full production LSM or jp-meta; the percentages
must not be treated as guaranteed savings on the combined production databases.

Every record is parsed with the existing `serde_json` behavior, encoded and
decoded, then compared using exact MessagePack serialization of the typed
message. The private experimental schema includes factual snapshots, targets,
clear instructions, signals, frozen shares and fees, projections, source IDs,
timestamps and legacy leverage. It uses doubles and explicit scalar presence
where needed to preserve signed zero and optional values. A focused unit test
also covers signed zero, tiny quantities, absent USDT values, clear targets,
fee maps and matched-contract projections. Equality applies to the current
typed interpretation; it does not preserve original JSON spelling or unknown
JSON fields. Original sample bytes remain separate and unchanged.

Production still stores JSON. Adopting a binary format would require recoverable
original-byte backups, a coordinated replacement of every archive reader/writer
and maintenance tool, and financial-result validation including immutable
fingerprints. The experiment does not change global JSON number parsing, add
production format negotiation, or authorize a rewrite. Protobuf also retains
each repeated full factual snapshot: reducing that repetition is a separate
storage design change. Manual date-based archival remains useful for bounding
history growth regardless of encoding.

Evidence in the same private NVMe root: `codec-all-result.json`,
`codec-recent-result.json`, `codec-all/` and `codec-recent/`. Focused precision
tests, archive-client compilation and its release build passed.
