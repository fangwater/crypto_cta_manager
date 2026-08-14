# Repository Guidelines

## Project Scope

`crypto_cta_manager` is an independent Rust project for Exec/CTA order ingestion
and management. It is intentionally separate from
`/home/u171/fanghaizhou/crypto_nav_manager`, whose current responsibility is
NAV-oriented order/history collection. Large Exec-specific changes belong here;
do not couple the two projects through relative paths or shared runtime state.

The crate was initialized as a Rust 2024 binary. Keep application code under
`src/`, database migrations under `migrations/`, deployment files under
`deploy/`, and operational scripts under `scripts/` as those areas are added.

## Build And Test

Use the standard Rust workflow:

```bash
cargo fmt --check
cargo check
cargo test
cargo build --release
```

Run `cargo fmt` before committing Rust changes. Prefer focused tests while
iterating, then run the full crate tests when changing database schemas,
ingestion checkpoints, or shared order models.

## Database Topology

Each CTA Exec host owns a user-managed local PostgreSQL instance. That local
database is the primary store for the CTA instance's orders, fills, account
state, and synchronization checkpoints. Exec hosts do not require `sudo` for
PostgreSQL and must not write directly to the central database as their only
durable store.

The PostgreSQL instance on `el_dev` is the synchronization and aggregation
target for multiple CTA instances. It does not replace any Exec-local
PostgreSQL database. ClickHouse remains on `el_dev` for analytical data.

The central development host is:

```text
SSH alias: el_dev
HostName: 172.16.30.42
User: fanghaizhou
```

Passwordless login with `~/.ssh/id_ed25519` was verified on 2026-08-13 UTC.
Connect with `ssh el_dev`. Do not hard-code credentials in this repository,
commits, logs, or chat output.

## Multi-Account Order Ingestion

A single host-level manager may ingest multiple local Exec deployments, such
as `binance_exec_trade01` and `binance_exec_trade02`. Treat each deployment as
an independent source with a stable, globally unique `source_id`; never infer
identity only from an account label, path, process name, or array position.

Order primary keys, ingestion failures, and checkpoints must include
`source_id`. Each source runs as an independent worker so a missing/corrupt
RocksDB or a delayed account cannot stop the other accounts. Source paths must
be absolute and two enabled sources must not point at the same RocksDB.

The source reader opens the live `persist_manager` RocksDB read-only once per
poll and scans `uniform_orders` using half-open microsecond ranges. It must not
write into the RocksDB or require stopping a live trading process. PostgreSQL
writes are idempotent on `(source_id, record_key)`, and the order rows plus the
source checkpoint advance in one transaction. Retain a safety lag and overlap
window so records racing the minute boundary are re-read safely.

CTA Exec hosts write only to their local PostgreSQL primary. Future central
synchronization must preserve the same `source_id` and idempotency keys; it
must not turn `el_dev` into the only durable copy for an Exec account.

## CTA PnL Reconstruction

`nav_rebuild` reconstructs CTA PnL from a PostgreSQL position snapshot plus the
later local `uniform_orders` RocksDB history. PostgreSQL is only the immutable
recomputation anchor; order events remain sourced directly from RocksDB. A pure
RocksDB diagnostic mode may bypass snapshots. Positive `amount_update` values
are base-quantity fills, and each estimated fee is
`price * amount_update * estimated_fee_rate`.

All venue types use quantity FIFO. Keep FIFO state isolated by `source_id`,
symbol, and event venue; never close a position with a fill from another account
or venue. Order fills by `update_ts_us` and use the RocksDB receive key only as a
stable tie-breaker. Reports aggregate only after each isolated FIFO has been
evaluated. Store timestamped initial-position snapshots in PostgreSQL, never in
TOML. Insert those signed quantities as zero-volume, zero-fee FIFO lots. An
omitted reference price uses the first positive RocksDB fill for the same symbol
and venue. There is still no baseline equity: report NAV change from zero at the
snapshot rather than absolute NAV. Until an external mark feed is connected,
value each open venue position at its latest fill price and identify that mark
source in output. Order-only reconstruction does not include funding, deposits,
withdrawals, liquidation charges, or other account-ledger movements; keep those
limitations explicit.

## CTA Dashboard

The CTA dashboard is a React/Vite application under `frontend/`, backed by the
read-only `cta_web` API. The API refreshes its in-memory report every 60 seconds
from local PostgreSQL snapshots plus later RocksDB fills. It keeps the last good
report when a refresh fails and must never expose database credentials or write
to the live RocksDB.

The main NAV display is a time series, not a per-symbol contribution bar chart.
`GET /api/timeline` rebuilds it on demand and accepts camel-case `startMs`,
`endMs`, comma-separated `sourceIds`, comma-separated `symbols`, and
`maxPoints`. Process every post-snapshot fill before `startMs` to establish the
isolated FIFO state, capture that state as the window baseline, and report only
changes inside the selected window. A fill exactly at `startMs` belongs to the
window. Include explicit start and end points, resample to fixed 15-minute step
points, and preserve extrema when a response must be downsampled. The browser
must expose custom datetime-local start/end controls, `ALL`/`1D`/`7D`/`30D`
quick ranges, aggregate and per-symbol curves, account scope, and symbol
all/none selection.

CTA timeline series are NAV before estimated fees, NAV after estimated fees,
realized PnL, floating PnL, and estimated trading fees. There is no baseline
equity, funding, interest, deposit, or withdrawal series. The first point at a
normal window boundary is zero; the final point must equal the selected-window
summary. PostgreSQL snapshots remain immutable source-specific initial-position
anchors. Never merge FIFO state across source ID, symbol, or venue.

The unified gateway below was deployed and verified on 2026-08-14 UTC. At that
time only `binance_exec_trade01` existed on the CTA Exec host; do not assume a
`trade02` upstream exists without checking the remote deployment and listeners.

On `binance_exec_trade01`, `cta_web` binds to `127.0.0.1:18201` and
`crypto-cta-nginx.service` binds to `127.0.0.1:10051`. Both are active
`systemd --user` services. The Nginx service is the single loopback gateway and
must not replace, restart, or reconfigure the existing Viz service on port
`10041`, its Config service on port `18161`, or any trading process.

The deployed Nginx routes are:

```text
/                         -> 302 /manager/
/manager/                 -> CTA Manager frontend
/manager/api/             -> cta_web /api/
/exec_trade01/            -> Exec Viz
/exec_trade01/ws          -> Exec Viz WebSocket
/exec_trade01/snapshot    -> Exec Viz snapshot
/exec_trade01/config/     -> Exec Viz's Config proxy
/cta/                     -> compatibility redirect to /manager/
/cta-api/                 -> compatibility proxy to cta_web /api/
```

Keep `absolute_redirect off` in this Nginx server. Without it, redirects expose
the loopback gateway port `10051`, which is not reachable through the external
single-port entry. Preserve the WebSocket `Upgrade` and `Connection` headers,
disable proxy buffering on streaming/API routes, and retain a long read timeout.
The WebSocket is the long-lived real-time Viz channel; HTTP serves the page,
snapshot, and Config requests. An end-to-end WebSocket handshake through the
gateway returned `101 Switching Protocols` during deployment verification.

The single internal entry point is `http://172.16.30.42:10041`. On `el_dev`,
`cta-exec-gateway-tunnel.service` forwards that address to the Exec host's
loopback Nginx on port `10051`; it must not bypass Nginx by forwarding directly
to Viz. The normal unit is installed under `~/.config/systemd/user/`, is active
and enabled, and replaced the old transient `el01-exec-viz-tunnel.service`.
Linger is enabled for `fanghaizhou`, so the user manager can start the tunnel
after reboot without an interactive login. Keep the real SSH destination only
in `~/.config/crypto-cta-manager/gateway-tunnel.env` on `el_dev`, with mode
`0600`; never copy it into this repository, logs, or chat.

Additional Exec accounts receive distinct path prefixes and loopback Viz
ports. After an account is actually deployed, add one named Nginx upstream and
one account-prefixed proxy location; the external port and SSH tunnel remain
unchanged. Never route two account prefixes to the same upstream by accident.

The current `cta_web` API is read-only. Nginx already preserves request methods,
bodies, and response statuses so future Manager endpoints can update
Redis-backed strategy configuration without another gateway change. Do not let
the browser connect directly to Redis. Implement writes in the Manager API with
authentication, source/account scoping, input validation, and an audit trail;
then expose them below `/manager/api/`.

End-to-end HTTP checks from both `el_dev` and the development host returned
`200` for Manager, Manager API, Exec Viz, snapshot, and Config. The development
host has a global HTTP proxy that can intercept private-address curl requests;
use `curl --noproxy '*'` (or an appropriate `NO_PROXY`) for direct checks.
Desktop and mobile browser-render checks were subsequently completed with
Chromium on `el_dev`; remove their screenshots and temporary browser profiles
after verification instead of leaving them in the deployment environment.

The timeline version was deployed on 2026-08-14 UTC. A real all-history query
returned 452 fixed 15-minute points for 12 symbols: its start point was zero,
its final point matched the window summary, and NAV-before-fee minus
NAV-after-fee equaled the estimated fee total. A one-day BTC-only query returned
one symbol series and a matching aggregate summary. Exec Viz and Config still
returned HTTP 200, and the proxied Viz WebSocket returned `101 Switching
Protocols`. Only `crypto-cta-manager-web.service` was restarted; Nginx and all
trading processes remained running. Upload to timestamped `.next` paths, verify
checksums, switch atomically, and remove deployment staging files, temporary
backups, test screenshots, and browser profiles after successful verification.
Resolve and delete exact paths only, then confirm they are absent; do not use
broad globs or recursive cleanup rooted at a runtime directory.

Install the pinned Ubuntu Noble Nginx package without sudo through
`scripts/install_nginx_user.sh`. Its default mirror is Tencent Cloud and its
package checksums are fixed in the script. Deployment files live under
`deploy/binance_exec_trade01/`.

## Central PostgreSQL Layout

el_dev runs the native PostgreSQL 16 cluster `16/cta` with the following
verified storage layout:

```text
Root:        /mnt/Data/postgresql
Data:        /mnt/Data/postgresql/16/cta
Unix socket: /mnt/Data/postgresql/16/run
Logs:        /mnt/Data/postgresql/16/log
```

PostgreSQL 16.15 was installed and verified on 2026-08-13 UTC. The cluster is
online, owned by `postgres`, and enabled through `postgresql.service`. It
listens on `127.0.0.1:5432` and `172.16.30.42:5432`. The application role and
database are both named `crypto_cta_manager`; TCP authentication uses SCRAM.
Remote access is restricted to that role/database pair from
`172.16.30.38/32`. Credentials are stored only on el_dev in
`~/.config/crypto-cta-manager/database.env` with mode `0600`.

el_dev reports Ubuntu 22.04 Jammy, but its Ubuntu base APT entries still target
Focal. PostgreSQL packages come from the Tencent Cloud `jammy-pgdg` mirror with
the official PostgreSQL signing key. Do not run a broad package upgrade or
rewrite the host's base APT sources without explicit operator approval.

## Known CTA Exec PostgreSQL

Connect to the first known CTA Exec with `ssh cta_exec`. Always re-check
`hostname -f` and the runtime directory before operating because this is a live
trading host. The expected runtime is `/home/el01/binance_exec_trade01`; never
read or print its `env.sh`, and do not start, stop, or deploy trading processes
as part of database maintenance.

PostgreSQL is installed entirely under the `el01` account and binds only to
loopback. The following layout was deployed and verified on 2026-08-14 UTC:

```text
Version:      PostgreSQL 16.15
Binary:       /home/el01/.local/opt/postgresql/16
Data:         /home/el01/.local/var/lib/postgresql/16/cta
Unix socket:  /home/el01/.local/run/postgresql
Logs:         /home/el01/.local/var/log/postgresql
TCP:          127.0.0.1:15432
User unit:    postgresql-cta.service
Credentials:  /home/el01/.config/crypto-cta-manager/database.env
```

The application role and database are both `crypto_cta_manager`. The credential
file uses `CRYPTO_CTA_LOCAL_DATABASE_URL`, is mode `0600`, and must never be
copied into this repository, logs, or chat. Manage the server with
`systemctl --user`; lingering is enabled for `el01`, so the unit starts without
an interactive login. The source build is intentionally loopback-only and omits
TLS, readline, and compressed client tooling. The source build and installer
are respectively `scripts/build_postgresql_user_bundle.sh` and
`scripts/install_postgresql_cta_exec.sh`.

## Central ClickHouse Layout

el_dev already has a user-managed ClickHouse instance with this verified
layout:

```text
Root:           /mnt/Data/fanghz/clickhouse
Binary:         /mnt/Data/fanghz/clickhouse/clickhouse
Config:         /mnt/Data/fanghz/clickhouse/config
Data:           /mnt/Data/fanghz/clickhouse/data
Logs:           /mnt/Data/fanghz/clickhouse/log
Runtime files:  /mnt/Data/fanghz/clickhouse/run
Temporary data: /mnt/Data/fanghz/clickhouse/tmp
User files:     /mnt/Data/fanghz/clickhouse/user_files
Format schemas: /mnt/Data/fanghz/clickhouse/format_schemas
```

The configured endpoints are loopback-only on el_dev: HTTP
`127.0.0.1:18123` and native TCP `127.0.0.1:19000`. The data directory was
confirmed through `system.disks` as `/mnt/Data/fanghz/clickhouse/data/` on
2026-08-13 UTC. Run ClickHouse commands through `ssh el_dev` or an explicit SSH
tunnel; do not expose these ports or assume the conventional 8123/9000 ports.

## Data Safety

Treat both databases as persistent trading infrastructure. Default to
read-only inspection, use migrations for PostgreSQL schema changes, and keep
ClickHouse DDL explicit and reviewable. Never drop, truncate, replace, or bulk
rewrite remote data unless the user explicitly authorizes the exact host,
database, tables, and operation. Before any mutating remote command, state the
target and expected impact and take a recoverable backup when applicable.
