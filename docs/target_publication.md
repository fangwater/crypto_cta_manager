# Target publication

Last updated: 2026-11-07. The archive write-behind implementation below is deployed
to jp-meta. el01 retains the previous implementation, which awaits the RocksDB
WAL before Exec writes. The deployment records distinguish the two rollouts.

An existing strategy's `POST /api/catalog/position-strategies` does not query or
write PostgreSQL. Manager checks the prepared in-memory session/publish-token
permissions, order templates, bindings, startup account modes and target
projections. It freezes each binding's shares, projection and fees, updates the
memory target and queues both storage writes without waiting for either store.
Exec writes run directly from the received target and frozen metadata; the
existing confirmed Redis write/readback and reload-notify
contract is unchanged. All bound account writes complete before returning 200.
Same-strategy requests remain ordered through all their account writes, while
different strategies may proceed concurrently. An admitted publish survives
an HTTP client disconnect.

Both stores use one wake/flush/retry worker pattern, with independent tasks so a
stalled store cannot stop the other. PostgreSQL uses its asynchronous sqlx I/O;
the RocksDB worker awaits `spawn_blocking` away from the request. Merely awaiting
`spawn_blocking` in the request would free the runtime thread while still delaying
Exec, so the request only enqueues memory work. The archive queue preserves every
signal in FIFO order and retains its front on errors. It holds at most 512 pending
messages, including the in-flight write. Saturation or shutdown returns503 before
changing memory or publishing, so history is never silently dropped to make room.
One worker performs synchronized atomic current-target/history WAL batches;
retrying an already committed target does not append duplicate history. PG may
finish ahead of RocksDB and always coalesces only the latest target per strategy.

HTTP200 confirms the Exec writes, not storage durability. A crash, forced kill,
or expired shutdown timeout can lose queued history and any current target that
neither store has saved yet. Startup restores only saved records and never
invents missing history. Normal shutdown closes admission and waits up to ten
seconds for queued archive writes after HTTP completion; a failed drain is logged.
Health exposes pending archive/mirror counts and a persistence-error flag;
background write errors mark health degraded and retain the work for retry.

PG `cta_position_strategies.targets`, overrides and `updated_at_us` are a
background latest-state mirror. Pending writes are coalesced by strategy, so
an outage does not create an unbounded per-message queue. A delayed write cannot
replace a newer version, recreate a deleted row, or update a new strategy that
reuses an old name. The original catalog creation time identifies that row.
PG mirror failures are logged and retried; they do not change a successful Exec
HTTP response. Normal target acknowledgements do not wait for the mirror.

The eventually written RocksDB `position_updates` history remains the target input for theoretical
TWAP/VWAP, with frozen shares, projection and fees. One `position_current:<name>`
key in the existing default column family holds its recoverable current target
and catalog identity. Startup overlays a newer RocksDB target on the PG mirror
and schedules the mirror to catch up. A configuration-only archive event or
zero-share binding event does not overwrite the strategy's current template.
No historical targets are replayed automatically into Exec during startup.

Exec Viz factual positions are optional, bounded background enrichment after
the Exec writes. They wait for their archive event only in the background before
enriching it. Their actual observation timestamps remain attached; they are
not represented as observations made at target receipt. Snapshot timeout or
missing enrichment stays unknown and cannot delay a live target. The theoretical
target reader already excludes these factual observations.

Configuration remains durable in PostgreSQL. Startup must load a complete
permission/order/binding/fee/session snapshot before opening target admission.
Normal configuration APIs invalidate that snapshot under the configuration
write lease, and a worker reloads it before reopening admission. Targets return
503 promptly while configuration is being edited or reloaded, rather than
waiting behind its SQL. In-flight admitted publications finish before the edit;
subsequent requests cannot use revoked access or stopped bindings. Correcting
configuration by external SQL requires a Manager restart to reload the snapshot.

Session-authorized creation of a previously unknown strategy is a cold
configuration operation and still needs PG. It inserts only an absent name and
rechecks the resulting ownership; a machine publish token cannot create one.
Existing targets continue using the memory catalog while a new name is being
created. Source/account-mode configuration retains its startup-only reload rule.

Catalog lists, previews, manual republish, binding changes and virtual activation
overlay the current memory target on any older PG target. They must not publish
the delayed mirror's quantity. A missing/failed startup account-mode check still
blocks nonzero targets; zero clears remain allowed. Redis/Exec publication
failure returns 502 and leaves the accepted target in memory and its background
storage queues for a caller retry. Background workers only persist records;
they never automatically replay old signals into Exec. Configuration-only
archive operations also offload RocksDB calls to blocking workers. Startup and
catalog reload read RocksDB off the runtime thread and before acquiring memory
locks used by live publications.

## JP production deployment

Published `b8eb962` on 2026-11-06 as `target-pg-async-20261106T152335Z`.
The task-only commit was pushed to `origin/main`; pre-existing worktree changes
were preserved. All 81 source files in the previous JP release manifest matched
the pre-task worktree baseline. The build retained that already-published source
baseline and added this patch; the release manifest records every source hash.
Built locally in the existing checkout with
`cargo build --locked --release --no-default-features --bin cta_web`.
The original JSON float parser and existing allocation fingerprints were retained.
The full suite passed 518 tests with 27 existing environment skips; all 37 web
tests, including isolated PostgreSQL and Redis cases, passed with fixtures enabled.

Uploaded the exact binary to an explicit temporary path, checked SHA-256 and
passed the read-only allocation preflight for six sources. Atomically installed
only `bin/cta_web` and restarted `crypto-cta-manager-web.service`; no schema
maintenance, frontend upload or trading-process restart was performed. Final
binary SHA-256:
`f4064c8b82d29509427225562261ce0f91e39fd83cf010c55fb29ee04106df4b`.
Manager PID `1475583`, all threads on CPU24, `NRestarts=0`. All eight enabled
publishable sources passed startup account-mode checks. The API became ready
at 15:31:19 UTC and the theoretical target index completed initialization.

Through 15:37:56 UTC, 785 natural target requests returned HTTP200, including
129 `rbf_small` requests. Overall median/P95/max processing was 5/25/256ms;
`rbf_small` was 6/25/256ms. The independent gateway logged 759 post-startup
position POSTs, all200. There were no application errors, bound-account publish
failures or delayed-PG-mirror warnings in that observation window. The mirror
caught up to all 12 observed active strategies. A stable-revision read of both
`rbf_small` Chase runtime keys matched the template's 33 targets multiplied by
their respective shares; Batch keys are not its active execution path.

All 11 checked metadata/immutable-snapshot table digests, including 27 bindings
and 19 strategy identities, were unchanged. The 25 non-binary runtime file hashes,
frontend target and 215 other long-lived process identities/surviving thread
affinities were unchanged; one separately observed `sleep` auxiliary ended.
Redis PID2769589 and Delist PID1367608 retain CPU28. Anonymous Manager health
returns401 and the unchanged frontend returns200.

Remote backup: `~/crypto_cta_manager/backups/target-pg-async-20261106T152335Z/`.
Operational evidence: `~/crypto_cta_manager/run/target-pg-async-20261106T152335Z/`
and local `../artifacts/manager_async_targets_20261106/deploy/`. A rollback is
binary-only and must preserve current host configuration, Redis and RocksDB.

## JP archive write-behind deployment

Deployed `target-archive-async-20261107T000354Z` on 2026-11-07. Installed the exact
previously tested `--no-default-features` binary, preserving JP's JSON parser and
allocation fingerprints. SHA-256:
`46be509a1730b96923e22298b0b48029ec6046176176dd0d073766f1991eb87f`.
This is a worktree build with per-file source hashes, based on HEAD `737828b`;
it is not represented as a committed feature revision. Compared with the prior
production manifest, only `position_archive.rs`, `web.rs`, `web/target_state.rs`
and its publication regression tests differ. Both full feature-set suites passed
(521/523 tests, 27 existing environment skips each); all 39 Web tests passed with
isolated PostgreSQL and Redis, including publication while real archive writes
were blocked. Formatting, cargo check and the release build passed.

After the checksum and read-only allocation preflight passed, backed up the old
binary and atomically replaced only `bin/cta_web`. Restarted only
`crypto-cta-manager-web.service`. Manager PID2587978 retained CPU24 on all observed
threads, with NRestarts=0. All eight enabled publishable sources passed startup
account-mode checks. The API became ready at 00:08:19 UTC. The target index finished
initialization at 00:12:59 after 8,311,552 messages; final status was ready, not
loading, without an error.

Through 00:14:09 UTC, 677 natural publications returned200, including 114
`rbf_small` requests. Overall median/P95/max processing was 1/18/1566ms;
`rbf_small` was 3/22/42ms. The observation includes the natural 00:10 five-minute
batch. No target persistence, admission or bound-account publication errors were
observed. The PG mirror caught up to all nine observed strategies. Independently,
the gateway recorded 741 target POSTs, all200, from 00:08:20 through 00:14:43;
the different sampling endpoints are not a one-to-one request comparison. The
original sender logged HTTP success at 00:08:22 and again in the 00:10 batch.

Authenticated health was ok with both persistence queues empty and no persistence
error. Readback confirmed new `rbf_small` archive events and both Chase runtime
bindings, with all 66 scaled targets/signals matching and no revision race. The
166 checked protected process identities/surviving thread affinities, 50 runtime
file hashes, frontend target/contents and 11 catalog/immutable-snapshot digests
were unchanged. Temporary diagnostic sessions were deleted and verified absent;
the explicit temporary binary path no longer exists. No manual target was pushed.
Trading processes, frontend, host configuration and schema were preserved; Redis
was neither restarted nor reconfigured.

Rollback binary and configuration metadata:
`~/crypto_cta_manager/backups/target-archive-async-20261107T000354Z/`.
Evidence: the same release under `~/crypto_cta_manager/run/` and local
`../artifacts/manager_async_archive_20261107/deploy/`. Rollback remains binary-only;
preserve the current Manager RocksDB, Redis and host configuration. This rollout
removes the confirmed archive wait from publication; it does not establish that
every earlier request-completion gap had the same cause.

## el01 production deployment

Published the same target-publication patch `b8eb962` to `cta_exec` on
2026-11-06 as `target-pg-async-20261106T231853Z`. Built locally in the existing
checkout with `cargo build --locked --release --bin cta_web`, retaining el01's
default `daily-fifo` feature and JSON float-roundtrip parser. The complete
default-feature suite passed 520 tests with 27 existing environment skips;
formatting and the read-only allocation preflight passed. No schema maintenance
was needed. el01 currently has no allocation-anchor rows.

The previous el01 release's Cargo hashes and backend sources were compared with
the build manifest. Besides the publication patch, the three differing NAV files
contain the committed RapidX liquidity classification change `60ccb08` and its
daily-FIFO call signature. Reversing those changes reproduces all three previous
production hashes. All 14 enabled el01 sources use native Binance Futures, for
which that RapidX branch is inactive; the existing daily-FIFO implementation and
seven-day memory window were retained. No unrelated worktree edits were reset.

Uploaded to an explicit temporary binary path, verified SHA-256, backed up the
previous artifact and atomically installed only `bin/cta_web`. Restarted only
`crypto-cta-manager-web.service`; frontend, account bindings, source configuration
and trading processes were preserved. New binary SHA-256:
`aaedb4e930d96f3dcd72a4fce91c898348de9108dcfa3bbe3ae7d20b690da124`.

Manager PID `1440148` retained CPU0–7 on all 41 observed threads, with
`NRestarts=0`. All 14 enabled sources passed startup account-mode checks. The
API became ready at 23:26:33 UTC after its initial history load. Anonymous health
returns401 and the unchanged frontend returns200. The 36 non-binary runtime
file hashes, frontend contents and target, all 11 metadata/immutable-snapshot
digests (including 113 bindings), and 76 other process identities/surviving
thread affinities matched the pre-deployment baseline; 16 auxiliary threads
had ended between samples.

Through 23:31:35 UTC, 310 natural target requests, including the 23:30 five-minute
batch, returned HTTP200. Median/P95/max processing was 3/7/193ms. No application
errors, bound-account publish failures or delayed-mirror warnings were observed;
the PG mirror caught up to all 15 observed active strategies. Stable-revision
readback of 67 active Batch runtime bindings across 12 sources matched all 1,495
target quantities and execution signals, with no mismatches or read races.
No old signal was replayed or test target injected.

Remote backup: `~/crypto_cta_manager/backups/target-pg-async-20261106T231853Z/`.
Evidence: `~/crypto_cta_manager/run/target-pg-async-20261106T231853Z/` and local
`../artifacts/el01_async_targets_20261106/`. Rollback is binary-only and must
preserve current host configuration, Redis and RocksDB.
