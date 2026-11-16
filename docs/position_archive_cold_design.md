# Hourly archival with powerleader-initiated cold-store transfer

Operator direction, 2026-11-16: maintenance frequency must be hourly or slower.
Manager briefly uses a local cold store for archival; powerleader actively
acquires the cold maintenance lease, packages/pulls verified old data and
initiates removal of the source cold data. This supersedes the proposed direct
hot-prefix reclamation as the desired automatic design. It is a design target;
the automatic workers/local cold layout are not yet implemented or enabled.

## Locations and ownership

| Location | Responsibility | Writer lifetime |
| --- | --- | --- |
| Trading-host hot RocksDB | Current target recovery, normal publications and recent records | Manager owns the live handle |
| Trading-host cold RocksDB segments | Locally queryable historical signals | Manager opens only for a bounded archival batch, then flushes/syncs/closes |
| Powerleader NVMe | Verified long-term independent archives and offline analysis | Powerleader task owns its local output |

Retain at least 30 analysis days plus three redundancy days and the existing
execution guard across the locally queryable hot/cold history. This protection
applies when retiring local history. A hot-to-local-cold move retains that data
locally; it must not be confused with permanent local reclamation. NAV replay
requires a consistent reader and the boundary inventory/target checkpoint.

Prefer separately sealed daily cold segments, with bounded ordered ranges and
original source/DB identities. Powerleader can then retire complete old segments
without rewriting a large live cold DB. Normal RocksDB compaction still exists;
this does not promise zero disk I/O or instant physical file reclamation.

## Manager's local archival task

- Minimum interval 3,600 seconds; default one hour. Do not open cold stores on
  every signal, cache writable cold handles globally or run overlapping jobs.
- Start only after sustained low load: inspect Manager's actual CPU affinity,
  storage pressure and publication/archive backlog, not only host-wide CPU.
- Bound each batch's records/bytes and handle lifetime. Finish the current
  bounded batch, then yield when load rises. Do not promise interruption of an
  arbitrary already-running RocksDB operation.
- Copy the selected immutable prefix, verify it, sync cold WAL/data and commit a
  recoverable receipt before deleting corresponding hot history. Cross-DB
  writes are not atomic; keep originals until destination confirmation and
  make retries idempotent. Preserve every signal and its original ordering.
- Close all writable cold handles, iterators and snapshots at batch end. Query
  access uses bounded read leases, not a permanently open cold primary handle.

## Oldest-first batches

The operator requires at most one cold `WriteBatch` per archival attempt, with
a bounded record count. Also cap actual encoded batch bytes: one logical signal
can contain many observation/snapshot KV operations. Stop at the first of the
record limit, byte limit or destination-segment boundary. Do not write one batch
per record, hold an unbounded batch, or span multiple cold DBs in one attempt.
An individual record that cannot fit must remain in hot storage and report the
limit failure; it must not be skipped or silently exceed the configured budget.

Select the oldest eligible, immutable unarchived records in original
`(received_at_us, seq)` key order. Preserve original keys and source identities;
do not reorder by a mutable payload timestamp. Seek from the persisted cursor
on subsequent attempts instead of rescanning the entire archive. Freeze the
selected end key for the attempt so new arrivals cannot extend its scope.
Late insertions/enrichment must be excluded by the owner's seal/drain barrier
before confirming the batch immutable.

The one-batch limit describes the cold destination submission. Hot deletion is
a separate hot-DB batch after cold sync and readback confirmation; it cannot
share an atomic `WriteBatch` across two RocksDB directories. Commit removal of
the verified hot originals and the hot progress cursor together. If the cold
write succeeded but hot cleanup did not, retry the same keys idempotently,
reject conflicting destination data and never skip the unconfirmed prefix.

Batch caps and hourly cadence must support the actual incoming record/byte
rate, with headroom for catching up existing history. A single small batch per
hour below the arrival rate would continue growing the hot backlog. Measure
arrival and archival throughput/oldest-pending age before selecting limits;
do not promise bounded storage from a count limit alone.

## Powerleader's independent task

- Run hourly or slower, staggered from Manager's local archival task. Acquire
  the same source maintenance lock/lease; if busy, skip rather than pile up
  writers. The lease must survive independent restarts safely.
- Act through SSH/source-side helpers for the source filesystem. Do not mount
  a remote RocksDB directory and open it directly from powerleader.
- Select only sealed, unchanged cold segments strictly beyond the 33-day safe
  retention boundary and with no active query lease. Never manipulate hot DB
  files or remove a RocksDB `LOCK` to obtain access.
- Obtain a closed segment or consistent checkpoint, then release the source
  writable handle promptly. Network transfer operates on that stable view;
  it must not hold a cold writer open throughout a long copy.
- Package/transfer to NVMe, fsync and independently open/restore/read back the
  output. Compare source identity, ordered range, record count, hashes and
  retained reconstruction checkpoints before declaring it durable.
- After successful verification, reacquire the source lease, recheck the exact
  unchanged identity/range and query references, retire its catalog entry and
  remove only that verified source segment. Preserve the NVMe archive. A
  failed transfer or uncertain acknowledgement does not authorize deletion.

Powerleader owns the transfer/delete schedule; Manager does not push continuously
or require an always-on peer. When powerleader is unavailable, local archival
may continue, but source cold reclamation waits and capacity/backlog is exposed.

## Existing JSON and rollout

The deployed stores remain JSON. The prepared 33-day JSON owner supports a
manual backup/reclaim workflow; it does not implement this local cold store or
these two automatic tasks. The separate PB candidate requires an explicit
format migration and cannot simply be started on the current JSON history.
Existing JSON must be exported/converted with original-key, numerical and
retained-window verification before retiring its source. Avoid rewriting the
whole hot archive merely to prepare a cold backup.

No automatic timer, cold-store writer or remote reclamation is enabled by this
document. Implementation must retain bounded publication queues and recovery,
source/venue isolation, the 33-day floor, independent backup verification and
explicit host/database/operation authorization for live rollout.
