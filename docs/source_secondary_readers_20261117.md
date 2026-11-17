# Reusable Exec history readers — 2026-11-17

The jp-meta investigation found repeated read-only database opens replaying
retained Exec WALs before each incremental scan. In one approximately 41-second
observation, ten opens read 7,115,597,258 bytes inside the open calls and their
wall times summed to 32.416 seconds. Physical process read_bytes did not increase.
CRC32C was 21.15% of a separate short CPU sample. These are separate wall-time,
read-byte and sampling measurements, not interchangeable CPU percentages.

## Runtime behavior

`rocks_source::visit_column_families_from` now uses a bounded process-local
registry of reusable source secondaries. Dashboard refreshes, native historical
window restores, bounded full rebuilds and the RapidX evidence scans share it.
One-shot raw read helpers and the independent `cta_monitor` are unchanged.

The first access opens a secondary; subsequent accesses tail its primary via
`try_catch_up_with_primary`. Application scans retain their original receive-key
cursors and five-minute overlap, including late keys. Only requested, available
column families plus `default` are opened. The selected set grows when another
scan needs additional evidence CFs, without alternating opens on later passes.

Each source has its own mutex. Catch-up and all CF iterators for a scan hold that
mutex together: RocksDB secondary mode does not support ordinary snapshots.
Concurrent scans of that source wait for the current view; another source can
proceed independently. Existing immutable query generations and source/venue
financial separation remain in their current layers. No FIFO, fee or valuation
formulas change.

Reader validity includes the primary directory device/inode, original IDENTITY,
CURRENT manifest name and latest completed OPTIONS file metadata. OPTIONS and
manifest changes cause a fresh reader. This also handles ordinary primary
restarts and the important dropped/recreated same-name CF case: RocksDB otherwise
continues to expose a dropped handle's old data. Source metadata is checked before
and after opening/catch-up/scanning. Missing metadata, a changing source or a
storage error fails the read instead of returning a stale cached success.
Payload decoding failures retain the reader so a repaired value can be caught up
without recovering the old WAL again.

The original RocksDB 11.8.1 native library also has a separate recovery lifetime
defect: `ManifestTailer` retains pointers to CFs destroyed while recovering a
historical drop, and the first secondary catch-up can dereference them. A direct
library reproduction crashed independently of this reader pool. The project
vendors the same `librocksdb-sys` crate and removes the tracked pointer *before*
the base handler destroys the CF. No other upstream implementation file changes;
see [patch provenance](../vendor/librocksdb-sys/CTA_PATCHES.md).

Source directories are pinned through a directory descriptor and RocksDB receives
the corresponding `/proc/self/fd/` path. This prevents rocksdb-rs's unconditional
`create_dir_all` wrapper from recreating a removed live source pathname. Secondary
opens disable source creation and do not acquire the primary writer's lock.

## Ownership and resource limits

`cta_web` configures scratch at the Manager DB parent's `run/exec-readers`.
Configuration rejects overlap with Manager or any configured Exec database.
Each reader uses its own private temporary directory; closing it removes that
exact directory. The web lifetime guard closes admissions and retires the
registry on exit. Standalone history tools use private temporary directories.
No source contents are copied into a new durable order database.

The registry retains at most 32 cached readers and evicts inactive readers after
five minutes on a later access, or least-recently-used idle entries at capacity.
Busy readers are never evicted to make room. Each reader has one shared 8 MiB
block cache, a single initial file-opening worker and info logs capped at 1 MiB
per file with two retained files.

Selected SST descriptors use `max_open_files = -1` so primary flush/compaction
cannot remove a file needed by an active view. Before live rollout, check total
selected-file descriptor usage against the existing service limit. Such pins can
temporarily retain deleted SST space until catch-up/reader retirement; bounded
cache entry count is not a hard bound on SST count or secondary memtables. A
forced process kill can leave the small private scratch log directory, which is
disposable after that process has exited. No source flush, compaction, pruning,
WAL deletion or service restart is initiated by this reader.

## Validation and deployment

Focused regression cases cover live WAL appends and replacements, late keys,
reuse without reopen, omitted/added CFs, same-name CF recreation, primary restart,
primary flush/compaction, pinned multi-CF reads concurrent with atomic primary
writes, source replacement with a copied IDENTITY, disappearance during a scan,
exact overlap/head semantics, repaired payloads, unchanged primary file contents
and private scratch cleanup. A retained-WAL read-byte regression checks that
unchanged refreshes do not reread the original WAL. Full NAV tests remain necessary because the shared
reader feeds factual fills and liquidity evidence.

Completed validation: `cargo fmt --check`, `cargo check --all-targets`, full
`cargo test` (615 library and 18 binary tests passed; the existing 17
operator/benchmark and 19 isolated-PostgreSQL tests remain ignored), and
`cargo build --release`. All 15 focused reader tests also passed in release
mode. Thirty parallel debug repetitions passed after the native lifetime fix.
The local retained-WAL fixture read 8,394,782 bytes on initial opening versus
1,807 bytes across four unchanged refreshes, in both debug and release modes.
This is a synthetic no-new-data I/O measurement, not production CPU savings.

Predeployment read-only jp-meta metadata checks confirmed stable OPTIONS files
(approximately 65–571 hours old) and the unchanged old Manager PID 438574.
The candidate was subsequently deployed to jp-meta with an explicitly enabled
progressive PB/cold policy; see
[jp-meta deployment evidence](jp_meta_progressive_secondary_20261117.md).
The operator subsequently requested the same deployment on el01; see
[el01 deployment evidence](el01_progressive_secondary_20261117.md).
Validation logs and the source/artifact verification manifest are
retained under powerleader's NVMe archive directory.
