# Progressive JSON conversion and restrained compaction

Explicit `[archive.cold.hot_conversion]` implements the operator's latest
direction. Without it, existing JSON-only hot/cold mode remains available.
No production activation or Manager restart is performed by this implementation.

## Flow and recovery

On activation, Manager validates the source/cold destination, prepares split PB
families and atomically saves a conversion marker with the original archive
head. Startup rewrites no history. New signals immediately use PB, retaining
the synchronized current-target/history WAL batch. Only this durable transition
permits JSON/PB coexistence. Full reads, target scans, JSON pages and factual
enrichment share the rule. PB is never reparsed through JSON; converting JSON
enrichment preserves the original frozen target values.

The existing idle-gated minute worker alternates cold archival and hot conversion,
persisting its next turn and interval across restart. Process retained JSON oldest
first. Leave expired JSON for cold rather than rewriting it twice; when hot has
done no work, cold can run that turn. A large expired prefix is drained before
the hot cursor advances beyond it. New signals already use PB while this happens.
Keep 33 days plus the execution guard hot. Powerleader pull remains manual.

Hot conversion bounds scanned rows, aggregate original JSON bytes and final
WriteBatch bytes: defaults 1,000 records / 8 MiB / a 50 ms building budget. The
timer stops adding rows; one decode, validation or WAL sync can exceed it.
Oversized records remain unchanged and report an error. Each cold attempt still
submits at most one bounded destination WriteBatch with separate hot confirmation.

Before replacing JSON, a fresh storage reader reconstructs complete messages from
the actual staged CF operations plus existing immutable snapshot bodies. Exact
typed serialization must preserve all fields and floating-point bits, including
nulls, clears, identities, times, shares, fees and projections. Data and resume
cursor commit in one synchronized WAL batch. Original JSON spelling is not kept;
its complete validated typed content is retained. Post-commit errors can resume
without skipping records or reparsing PB. Busy readers/archive writers defer
conversion via nonblocking lock attempts; publication uses its independent queues.
Physical conversion changes no logical target revision or original key.

When the original suffix is converted/archived, mark completion. The running
process keeps transition decoding for old iterator values; after restart use
PB-only reads. An active transition requires explicit config in both Manager and
deployment preflight; the deploy script derives `--hot-conversion` from the table.

## CPU, I/O and capacity

No new busy loop or per-signal maintenance job is introduced. Require sustained
low CPU on Manager's actual affinity, low I/O PSI, no persistence backlog/error
and no running RocksDB background compaction. Work runs at low CPU/I/O priority.
Both input/output limits are retained so deduplicated snapshots cannot hide an
unbounded decoded-memory workload.

Conversion requests neither flush nor compaction. Automatic manual-range
compaction defaults **off**. Whole obsolete SST deletion after verified cold
archival remains available through RocksDB and rewrites no live data. Explicit
range-compaction opt-in has an independent persisted cooldown: default one hour,
minimum 30 minutes. Save the attempt before requesting it, preventing restart/error
bursts. Existing overlap, memtable, file-count, disk and reader checks still apply.

Normal background LSM compaction remains necessary; these are not hard CPU/time
caps. Later jobs wait while it is busy. Old JSON physical versions disappear over
time, not immediately on replacement. Monitor free space and pull/reclaim cold
segments manually; no never-full guarantee follows from bounded batches.

Cold retention previously invalidated the complete theoretical target index on
each prefix movement. That would repeatedly decode the remaining hot history
even with manual compaction disabled. The index now uses deque prefixes and
absolute offsets: retire each removed delta once, retain its exact cumulative
quantity as the market baseline, and keep stable retained-window curve revisions.
New appends use the same absolute offsets. True late inserts or an index behind
the retention floor still rebuild. Queries outside the available signal range
remain unavailable; cached old curves do not bypass that boundary.

## Rollout and observation

Use `deploy/position_archive/el01-progressive.example.toml` as a fragment for the
existing host config. It disables manual compaction and starts no network jobs.
No Exec stores or PostgreSQL schema/data are converted. One Manager restart is
still needed: Exec continues existing targets, while new target requests wait
until Manager finishes shutdown/startup checks. Startup performs no bulk rewrite.

Preserve binary/config and a recoverable Manager-owned checkpoint before live
activation. After PB events are accepted, use a compatible recovery owner and
preserve that newer DB. Do not overwrite its suffix with an older checkpoint
or start a JSON-only owner; the original checkpoint has no subsequent PB events.

`cold_archive.hot_conversion` exposes original head, cursor, counts, completion
and last batch data bytes immediately after opening. Actual batch checking includes
metadata; displayed data bytes exclude its marker. `last_action`, defer/error
state and the reclamation report show what work ran.

Tests cover partial conversion/restart, new PB/current targets, mixed/full reads,
row/byte/cursor limits, post-commit failure recovery, delayed enrichment, low disk,
busy readers/writers and expired JSON-to-cold. An independent original-JSON oracle
compares all four execution models' priced curves and acquisition/carry inputs
at every partial stage. Compaction tests cover default-off operation, persisted
cooldown and actual whole-SST reclamation.
Retained-month tests also verify no additional target messages are scanned on
prefix retirement, compare warm and fresh-cache curves, and test appends after
retirement for every execution model.

Final validation on 2026-11-16: `cargo fmt --check`, `cargo check --all-targets`,
deployment shell syntax and diff checks passed. Full Rust tests passed with
602 library and 18 binary cases; the existing 17 operator/benchmark and 19
isolated-PostgreSQL cases remain ignored. `cargo build --release` completed for
all binaries. Logs and the prepared release manifest are retained on powerleader
NVMe. Production remains unmodified by this feature rollout preparation.
