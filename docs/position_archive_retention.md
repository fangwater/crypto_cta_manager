# Manual position signal backup and RocksDB reclamation

Operator policy, 2026-11-15: use an explicit host and date for each manual run.
There is no automatic retention job, host iteration or scheduled compaction.
The earlier hourly service/timer templates have been removed. Powerleader's
previously staged timer remains disabled; no live signal pruning has occurred.

Only Manager's `position_updates` column family is eligible. Current targets,
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
   atomically saves its boundary checkpoint and deletes that prefix, then compacts
   only the already reclaimed `position_updates` range.

## Exact date semantics

`--before YYYY-MM-DD` means strictly before that date at 00:00 **UTC+08:00**.
For example, `--before 2026-10-01` excludes all records on/after
2026-10-01 00:00 Beijing time (2026-09-30 16:00 UTC). All same-timestamp sequence
numbers at the boundary are retained. Retrying never extends the date to today.

Manager rejects a date that would remove its retained monthly history. The
maximum safe boundary is UTC midnight at least 30 days ago minus ten minutes
for execution schedules and signal matching. An older manually chosen date
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
Original JSON bytes and 12-byte keys are neither converted nor deduplicated.
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
compaction is still requested. Manager rejects compaction beyond its verified
reclaimed boundary. Backup manifests remain available after success. A single
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
