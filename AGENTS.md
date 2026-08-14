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
