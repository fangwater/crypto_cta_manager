# Hourly archival with powerleader-initiated cold-store transfer

Operator direction, 2026-11-16: maintenance frequency must be hourly or slower.
Manager briefly uses a local cold store for archival; powerleader actively
requests a durable source-side package, pulls verified old files and
initiates removal of the source cold data. This supersedes the proposed direct
hot-prefix reclamation as the desired automatic design. The Rust workers and dedicated pull executable are implemented in this checkout.
They are opt-in; neither live host has been switched or scheduled.

## Locations and ownership

| Location | Responsibility | Writer lifetime |
| --- | --- | --- |
| Trading-host hot RocksDB | Current target recovery, normal publications and recent records | Manager owns the live handle |
| Trading-host cold RocksDB segments | Locally queryable historical signals | Manager opens only for a bounded archival batch, then flushes/syncs/closes |
| Powerleader NVMe | Verified long-term independent archives and offline analysis | Powerleader task owns its local output |

The implemented policy retains the full 30 analysis days plus three redundancy
days and the existing ten-minute execution guard in the hot DB. Only expired
records move to cold staging. Online readers therefore do not need a cold union
reader. NAV replay seeds from the exact retained boundary inventory/target
checkpoint. An empty or unavailable powerleader never authorizes data loss.

Use separately sealed cold segments within one Shanghai day, with bounded ordered ranges and
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
- Export a closed segment to a durable, atomically published gzip file on the
  source **before network transfer**. Group packages by Shanghai date and split
  a large day into bounded segment files. Reuse these exact files after a
  network failure; do not regenerate a streaming RocksDB export on each retry.
- SCP the finished file over the pinned public SSH endpoint. No RocksDB handle
  or source query lease is held during SCP. A failed file stays on the source;
  a local `.part` is never a verified backup. SCP retries the individual file
  from its beginning; it is not resumable rsync. An already complete, matching
  local `.part` can be verified without downloading again.
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

The deployed stores remain JSON. With explicit `[archive.cold]` configuration,
this owner selects the hot format once when opening the DB: an existing JSON
store continues writing JSON, while an existing/fresh PB store uses split PB.
Both export expired records as self-contained canonical PB cold segments,
including factual observations. There is no per-row format guessing or bulk
startup rewrite. Full hot JSON-to-PB migration remains a separate, explicit
offline operation. The deployment preflight accepts JSON only when cold mode
is explicitly configured; interrupted migrations and Exec stores are rejected.

No automatic timer, live cold-store writer or remote reclamation is enabled by
this document. Live rollout requires exact host/database/operation authorization.
Manager target publication remains on its independent bounded memory queues;
no source synchronization, cold-disk write or backup is awaited by publication.


## Configuration and executables

Build in the existing checkout/target directory:

```bash
cargo build --release --bin cta_web --bin cta_position_archive --bin cta_archive_pull
```

On the source host, opt in using its existing Manager TOML:

```toml
[archive.cold]
root = "/home/el01/crypto_cta_manager/cold-signals"
interval_secs = 3600
quiet_secs = 120
max_cpu_percent = 35.0
max_io_pressure_percent = 1.0

[archive.cold.limits]
max_records = 20000
max_bytes = 67108864
```

The root must be separate from hot/Exec stores, including resolved symlinks.
The cold directory is private (0700). The worker samples the busiest CPU in
Manager's actual affinity and Linux I/O PSI; missing telemetry or outstanding
archive persistence blocks maintenance. A dedicated low-priority thread runs
one attempt, with a persisted hourly attempt marker. It never explicitly forces
hot compaction. Hot space becomes physically free through normal RocksDB
compaction, so deletion is not an immediate disk-space guarantee.

Health exposes `cold_archive`: running/deferred/error state, recent count/bytes,
oldest hot timestamp, expired age and estimated hot key count. Batch selection
reserves half the configured bytes for rows and the remainder for exact boundary
checkpoints/metadata; the final actual WriteBatch must still fit the full cap.
Default capacity must be checked against measured live signal rates before
activation; these limits are not a promise of catching up on existing backlog.

Install only `cta_position_archive` beside the source's Manager. Install
`cta_archive_pull` on powerleader, and use
[`powerleader-cold.example.toml`](../deploy/position_archive/powerleader-cold.example.toml).
Load the host-only public endpoint environment before invoking the binary:

```bash
set -a
. /absolute/private/archive-pull/public.env
set +a
cta_archive_pull --config /absolute/private/archive-pull/powerleader.toml --host el01
cta_archive_pull --config /absolute/private/archive-pull/powerleader.toml --host el01 --run
cta_archive_pull --config /absolute/private/archive-pull/powerleader.toml --host el01 --run --reclaim-source --watch
```

Default invocation is a source inventory preview. One run handles at most one
segment; `--watch` waits at least an hour between attempts. Start a separate
host-selected process for jp-meta. No scheduler is installed automatically.
The dedicated Rust executable uses numeric **public IPv4 only**, `ssh -F
/dev/null`, strict host-key checks, no ProxyJump/ProxyCommand, no forwarding and
no private/default-route fallback. Endpoint values live in a private environment
file, never repository examples. Pin the known-hosts file from an existing
trusted connection. Metadata/package operations have a bounded lifetime (default 300 seconds);
SCP has its own `transfer_timeout_secs` (default 3600, maximum 86400).
Connected-but-stalled children are killed and reaped. Public loss is expected;
timeouts preserve the source package for the next scheduled file retry.

The source helper `cold-package` first verifies the closed cold DB, creates a
self-contained RocksDB gzip bundle under `.packages/YYYY-MM-DD/<segment-id>/`,
fsyncs and publishes `original.rocks.gz` with `package.json` atomically. This is
local export work under low CPU/I/O priority. Subsequent network attempts reuse
the file and its length/SHA-256 without rereading/exporting RocksDB. Manager's
hourly one-WriteBatch limit is unchanged: a daily folder can contain several
bounded files, without constructing a huge daily database write.

The Rust pull binary invokes **SCP/SFTP over the fixed numeric public IP** for
that exact immutable file. On powerleader it compares byte count and SHA-256,
restores the saved gzip into a temporary DB and independently verifies all file
hashes, ordered keys, canonical PB, counts and boundary replay. The local backup
directory is published atomically only after verification. Before deletion,
repeat gzip restore and verify the durable extracted DB. The source helper then
rechecks the exact cold receipt and deletes only that segment and its exported
package. Failed downloads never delete either source copy. Lost acknowledgements
and partial source-directory deletion recover using the saved receipt.

Powerleader keeps both `original.rocks.gz` and a queryable restored `db/` under
`<backup_root>/<host>/<segment-id>/`, together with `receipt.json` and the
post-deletion `source-removed.json`. These share the same local NVMe failure
domain; source deletion does not create independent hardware redundancy.

## Exec evidence is separate

This retention policy covers Manager position signals only. Exec persists
`uniform_orders`, `order_updates`, `trade_updates`, their unmatched report
families and `order_queue_positions` using native binary codecs. NAV consumes
uniform fills and raw trade evidence; the monitor consumes raw order/trade
activity and terminal state. Queue-position estimates are diagnostic and are
not inputs to Manager's core FIFO NAV. On non-colo el01, queue estimates and
receive latency are particularly sensitive to network/report delays; they do
not invalidate factual exchange fills. No Exec family is purged by this feature.
