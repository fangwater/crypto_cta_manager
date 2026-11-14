# Target publication

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
permission/order/binding/fee snapshot and session cache before opening target admission.
Publication configuration and authorization APIs invalidate that snapshot under the configuration
write lease, and a worker reloads it before reopening admission. Targets return
503 promptly while configuration is being edited or reloaded, rather than
waiting behind its SQL. In-flight admitted publications finish before the edit;
subsequent requests cannot use revoked access or stopped bindings. Correcting
configuration by external SQL requires a Manager restart to reload the snapshot.

Login, logout, registration and user creation do not take the configuration
lease or invalidate the publication catalog. Sessions have a separate memory
map: successful creation/deletion updates it directly, using the SQL expiration
time. An independent session-mutation mutex serializes those SQL/cache updates
with session reloads; publishers only read the memory map and never wait for that
mutex. Role changes and strategy/fallback-token permissions still use the
configuration lease and reload, so revoked publication access is not retained.
Funding settings and Virtual grant edits do not change the publication catalog.

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

Binance USD-M sources (`venue = "binance-futures"`) default their Exec account
order limits to 1,200 orders per minute and 300 orders per ten seconds, matching
Binance `exchangeInfo`'s `ORDERS` limits checked on 2026-11-11. Manager fills
missing fields in `<source_id>:<venue>:pre_trade_risk_params` on configuration
reads, and for enabled sources at startup and every 60 seconds in a background
worker. `HSETNX` preserves existing values, including an explicit zero, and all
other risk parameters. Explicit saves take precedence over defaults. Missing
hashes and partially populated hashes recover without a service restart; failed
sources are retried independently. Other venues do not inherit these Binance
defaults. Invalid existing values remain errors rather than being overwritten.
This maintenance does not join target publication or contact an exchange API.
