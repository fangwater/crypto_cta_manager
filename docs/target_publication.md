# Target publication

Last updated: 2026-11-06. This describes the implementation; production rollout
must be recorded separately after publishing a verified artifact.

An existing strategy's `POST /api/catalog/position-strategies` does not query or
write PostgreSQL. Manager checks the prepared in-memory session/publish-token
permissions, order templates, bindings, startup account modes and target
projections. It freezes each binding's shares, projection and fees, and records
the current target plus the historical event atomically in the Manager RocksDB
WAL. That small write is synchronized before execution so an acknowledged target
can be recovered. Exec writes then run directly from the received target and
frozen metadata; the existing confirmed Redis write/readback and reload-notify
contract is unchanged. All bound account writes complete before returning 200.
Same-strategy requests remain ordered through all their account writes, while
different strategies may proceed concurrently. An admitted publish survives
an HTTP client disconnect.

PG `cta_position_strategies.targets`, overrides and `updated_at_us` are a
background latest-state mirror. Pending writes are coalesced by strategy, so
an outage does not create an unbounded per-message queue. A delayed write cannot
replace a newer version, recreate a deleted row, or update a new strategy that
reuses an old name. The original catalog creation time identifies that row.
PG mirror failures are logged and retried; they do not change a successful Exec
HTTP response. Normal target acknowledgements do not wait for the mirror.

The RocksDB `position_updates` history remains the target input for theoretical
TWAP/VWAP, with frozen shares, projection and fees. One `position_current:<name>`
key in the existing default column family holds its recoverable current target
and catalog identity. Startup overlays a newer RocksDB target on the PG mirror
and schedules the mirror to catch up. A configuration-only archive event or
zero-share binding event does not overwrite the strategy's current template.
No historical targets are replayed automatically into Exec during startup.

Exec Viz factual positions are optional, bounded background enrichment after
the Exec writes. Their actual observation timestamps remain attached; they are
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
failure returns 502 and leaves the accepted target in RocksDB for a caller
retry. This is not a persistent automatic replay queue for old signals.
