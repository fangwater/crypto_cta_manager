# crypto_cta_manager

`crypto_cta_manager` reads order events from one or more local CTA Exec
`persist_manager` RocksDB databases. It supports both PostgreSQL order ingestion
and a PostgreSQL-independent CTA PnL/NAV-change reconstruction. Neither path
creates Parquet or other export files.

Each configured source is isolated by a stable `source_id`. PostgreSQL order
keys and ingestion checkpoints include that ID, so accounts such as `trade01`
and `trade02` can be collected by one process without key collisions. A failed
source retries independently and does not stop the other source workers.

## Configure

Create a deployment-local config based on
`config/cta-manager.example.toml`. The file must use absolute RocksDB paths.
Keep the PostgreSQL URL in the host credential file:

```bash
set -a
source ~/.config/crypto-cta-manager/database.env
set +a
```

The default URL variable is `CRYPTO_CTA_LOCAL_DATABASE_URL`.

Set `estimated_fee_rate` on every source used for NAV reconstruction. The value
is a decimal rate applied to every fill's `price * amount_update`; for example,
`0.0004` is 4 bps. The order-ingestion process does not require this setting.

If the account already had positions when its RocksDB history began, store an
immutable position snapshot in PostgreSQL with `nav_snapshot`. Position
snapshots are recomputation anchors and are deliberately not deployment config.

## Run

Apply migrations and verify source registration without reading RocksDB:

```bash
cargo run --release --bin crypto_cta_manager -- \
  --config config/cta-manager.toml --migrate-only
```

Run one poll for a deployment smoke test:

```bash
cargo run --release --bin crypto_cta_manager -- \
  --config config/cta-manager.toml --once
```

Run the minute-frequency workers continuously:

```bash
cargo run --release --bin crypto_cta_manager -- \
  --config config/cta-manager.toml
```

The first poll backfills all available history unless a source specifies
`start_ts_us`. Later polls use independent PostgreSQL checkpoints and a recent
overlap window. Inserts are idempotent on `(source_id, record_key)`, and the
orders plus checkpoint commit in the same PostgreSQL transaction.

## Rebuild CTA PnL

Store a position snapshot atomically. Quantities are signed base quantities;
the optional fourth field is a reference price:

```bash
cargo run --release --bin nav_snapshot -- \
  --config config/cta-manager.toml \
  --source binance_exec_trade01 \
  --snapshot-ts-us 1786284924397000 \
  --position BTCUSDT:1:-0.017 \
  --position ETHUSDT:1:-0.572
```

Snapshots with the same `(source_id, snapshot_ts_us)` cannot be overwritten.
Create a later snapshot for a new recomputation point. If a position omits its
reference price, `nav_rebuild` uses the first later positive RocksDB fill for
that symbol and venue.

Rebuild all enabled accounts directly from their complete RocksDB order history:

```bash
cargo run --release --bin nav_rebuild -- \
  --config config/cta-manager.toml
```

Select one or more accounts by stable source ID:

```bash
cargo run --release --bin nav_rebuild -- \
  --config config/cta-manager.toml \
  --source binance_exec_trade01 \
  --source binance_exec_trade02
```

The command loads only the latest position snapshot for each selected source
from PostgreSQL, then reconstructs exclusively from later RocksDB fills. It does
not read order events from PostgreSQL. Use `--no-position-snapshot` for a pure
RocksDB diagnostic rebuild.

Every snapshot position is inserted first as a zero-volume, zero-fee FIFO lot.
Every later positive `amount_update` is then treated as a fill in base quantity.
FIFO lots are isolated by `source_id`, symbol, and event venue;
accounts and venues never close each other's positions. Fills are ordered by
`update_ts_us`, with the RocksDB receive key used to break ties. The report
contains initial-position metadata plus venue, symbol, account, and
cross-account totals for volume, realized PnL before fees, estimated fees,
realized PnL after fees, floating PnL, and NAV change before and after fees.

There is deliberately no baseline equity: `nav_change_*` starts from zero at
the initial-position snapshot. Snapshot lots do not add historic fees, volume,
or pre-snapshot PnL. Open lots are marked at the latest fill price for the same
source, symbol, and venue. Monetary fields ending in `_quote` are
`price * quantity` values and assume the aggregated instruments use a compatible
quote or settlement currency.

Order history alone cannot reconstruct deposits, withdrawals, funding payments,
liquidation charges, or absolute account equity. Those values are outside this
first CTA-specific estimate.

## CTA Dashboard

The dashboard follows the operational layout of `crypto_nav_manager` while
keeping CTA data isolated by source, symbol, and venue. `cta_web` loads the
latest position snapshot for every enabled source from local PostgreSQL,
rebuilds quantity FIFO from the later RocksDB fills once per minute, and keeps
serving the last good report if a later refresh fails.

Build the API and frontend locally:

```bash
cargo build --release --bin cta_web
cd frontend
npm install
npm run lint
npm run build
```

The known Exec deployment uses these loopback-only endpoints:

```text
CTA API:       127.0.0.1:18201
User Nginx:    127.0.0.1:10051
Manager:       /manager/
Manager API:   /manager/api/
Exec Viz:      /exec_trade01/
Exec Config:   /exec_trade01/config/
```

Nginx is installed without `sudo` by `scripts/install_nginx_user.sh`. The
script extracts pinned Ubuntu Noble packages under `~/.local/opt/nginx`, uses
the Tencent Ubuntu mirror by default, and verifies both package hashes before
installing. The service files and Nginx configuration for the first account are
under `deploy/binance_exec_trade01/`.

Keep the service loopback-only like the existing Exec Viz deployment. Access it
through an SSH tunnel:

```bash
ssh -N -L 10051:127.0.0.1:10051 cta_exec
```

Then open `http://127.0.0.1:10051/manager/`. The same Nginx entry point also
proxies the Exec dashboard, WebSocket, snapshots, and configuration service
below `/exec_trade01/`. Add another account as a separate Nginx upstream and
path prefix only after that account's local Viz port has been deployed.

`el_dev` exposes the same gateway through one internal port. Its user service
forwards `172.16.30.42:10041` to the loopback-only Nginx listener above; the
SSH destination is stored only in
`~/.config/crypto-cta-manager/gateway-tunnel.env` on `el_dev`. Browser routes
are therefore:

```text
http://172.16.30.42:10041/manager/
http://172.16.30.42:10041/exec_trade01/
http://172.16.30.42:10041/exec_trade01/config/
```
