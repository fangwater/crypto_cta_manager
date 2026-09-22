# crypto_cta_manager

`crypto_cta_manager` reads order events from one or more local CTA Exec
`persist_manager` RocksDB databases. It supports both PostgreSQL order ingestion
and a PostgreSQL-independent CTA PnL/NAV-change reconstruction. Neither path
creates Parquet or other export files. `cta_web` can also keep a Manager-owned
TWAP archive from `spread_pbs` BBO; that RocksDB is separate from Exec.

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

The Manager order-strategy catalog supports `batch`, `pov`, and `chase`
templates. Batch and POV publish to the source-scoped `batch_exec:*` namespace;
Chase publishes its strict configuration to `chase_exec:*`. Per-symbol order
templates may switch between Batch and POV, but they cannot cross into Chase
inside one binding because the two execution families keep independent
position ledgers. A published binding can switch between the two families:
Manager records an `exec_switch:*` request, Exec freezes the old target, cancels
and reconciles every working child, transfers the binding's net position into
the destination ledger, and only then activates the new algorithm. The exchange
position is not flattened. Another strategy in the old family may not retain
the same account-symbol while the destination family takes ownership.

POV and Chase are experimental algorithms. Saving either algorithm, selecting
one for a binding or symbol override, and re-enabling a stopped experimental
binding requires the HTTP header `X-Experimental-Algorithm-Token: testtest`.
Stopping an experimental binding and switching it back to Batch remain
available without the token. The browser sends this header from its password
input and never stores the token in PostgreSQL or Redis.

## Login and account permissions

`cta_web` protects all Manager APIs with an HttpOnly session cookie. On a new
database, open `/manager/` and register the first user; that account is made an
administrator automatically. Later registrations create ordinary users with
no visible accounts until an administrator grants access.

Administrators can open `/manager/admin/` to promote users and select the
configured `source_id` values each ordinary user may view. Dashboard, NAV
timeline, execution-cost, acquisition-cost, account details, and Manager
position-update responses are filtered on the server. The machine-to-machine
`POST /api/catalog/position-strategies` push endpoint is intentionally direct
and does not require a browser session; catalog reads, deletes, account
settings, Exec Config updates, and permission changes still require an
administrator session. Direct Exec Viz and Config gateway routes also
validate the same session and source permission.

The password is stored as a salted PBKDF2-HMAC-SHA256 record in Manager's
PostgreSQL; session tokens are stored only as SHA-256 hashes. Do not copy the
database credential file or session values into this repository.

Set `maker_fee_rate` and `taker_fee_rate` on every source used for NAV
reconstruction. Both accept any finite decimal rate; negative values represent
rebates. Each fill uses `price * amount_update * role_fee_rate`. Liquidity role
comes from the exchange trade update `is_maker` flag, with order type used only
when raw role data is unavailable. The order-ingestion process does not require
these settings.
Set an explicit one-segment `gateway_prefix`, such as `/exec_trade01`, for each
account whose Exec Viz and Config services are exposed through the unified
gateway. The dashboard never derives service paths from account names.

`cta_web` keeps a host-global Manager RocksDB at `twap.rocksdb_path`, default
`/home/el01/crypto_cta_manager/db`. This is not an Exec-account store, so it
must not live under `binance_exec_trade01` and must never reuse an Exec
`persist_manager` path. Each accepted `POST /api/catalog/position-strategies`
is also appended as one JSON message in column family `position_updates`.
Setting binding shares to zero appends an additional stop message before the
zero target is published. PostgreSQL remains the current catalog; RocksDB is
the append-only position-publication history. Each archived message records
the affected account's then-current `shares`, and factual positions from its
Exec Viz
`/snapshot` `exec_pre_trade_state` row (`current_qty` for that strategy).
Published qty is reconstructed later as template qty × shares. Later share
edits must not be used to reconstruct an older fill. Set
`exec_viz_url` to the loopback Viz origin, such as `http://127.0.0.1:10041/`. When `[twap]` is enabled, the same database also records
5-second mid TWAP bars for catalog symbols from
`spread_pbs/<venue>/ask_bid_spread`. TWAP uses one compact binary column family
per `SYMBOL:venue`. Bars older than `retain_days` are deleted and compacted;
position-update messages are not compacted by that job.

## CTA Health Monitor

`cta_monitor` is an independent process that checks the public BBO stream, the
recent read-only Exec order records, and each Exec Viz pre-trade position
snapshot. It reports stale market data, stalled live orders, unmatched order or
trade updates, stale/not-ready position state, and inconsistent or overdue
position execution to DingTalk. It never writes to Exec RocksDB, PostgreSQL,
Redis, Exec Config, or an exchange API.

Configure `[monitor]` and `[monitor.dingtalk]` in the host TOML. Set the market
webhook in `CTA_DINGTALK_MARKET_WEBHOOK_URL` and the order/position webhook in
`CTA_DINGTALK_ORDER_WEBHOOK_URL` in the host `EnvironmentFile`; optional
signing secrets use the per-channel environment names in the TOML. Do not put
the full webhook URLs in this repository.

```bash
cta_monitor --config /home/el01/crypto_cta_manager/config/cta-manager.toml \
  --once --dry-run
```

After reviewing the dry-run output, set `monitor.enabled = true` and start the
independent monitor under pmdaemon with `scripts/start_monitor.sh`. Each
channel retries failed
webhook requests with exponential backoff. Notifications are sent on state
transitions, repeated at `repeat_alert_secs`, and followed by a recovery notice;
the monitor does not send every poll.

`GET /api/catalog/position-updates` returns a raw JSON page from
`position_updates` (default `limit=100`, maximum `1000`). Each array member is
emitted from the raw JSON stored in Manager RocksDB, without rebuilding or
rescaling historical targets. To continue, set `afterUs` and `afterSeq` to the
`received_at_us` and `seq` of the preceding page's final member.

`GET /api/catalog/execution-cost` generates an on-demand report. It is not a
real-time job. Each archived position update's intended qty is template qty ×
the shares stored in that message minus the snapshot qty. The
default execution window is 5 minutes (`windowSec`, later adjustable) and ends
early at that same source and binding's next publication. Assume the intended
qty is executed uniformly over that window. Split from the update timestamp
into consecutive 1-minute buckets; each 1-minute mid is the equal average of the 5-second mid
bars in that bucket, then those 1-minute mids are averaged. A 5-minute window
therefore uses five 1-minute mids. The latest non-stale completed 5-second mid
at the update is `arrival_mid`. For windows with actual fills, price execution
uses the same signed filled quantity for both paths: actual slippage is
`filled × (VWAP − arrival_mid)`, TWAP slippage is
`filled × (twap_mid − arrival_mid)`, and shortfall versus TWAP is
`filled × (VWAP − twap_mid)`. Positive values mean worse execution for both
buys and sells. Estimated maker/taker fees are reported separately and never
included in these price metrics. Fills come from that account's Exec
`uniform_orders` and are attributed with `batch_exec:<strategy_name>` or
`chase_exec:<strategy_name>`; both prefixes map to the same stable Manager
binding name. Only
messages that archived `published_accounts` (with each account's `shares`) are
included. The browser page is `/manager/execution-cost/`.

If the account already had positions when its RocksDB history began, store an
immutable position snapshot in PostgreSQL with `nav_snapshot`. Position
snapshots are recomputation anchors and are deliberately not deployment config.
For strategy PnL that follows Exec's actual allocation, create a later immutable
strategy-allocation snapshot with `nav_strategy_snapshot`; it becomes the new
recomputation anchor for that source.

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

## Rebase Strategy PnL

Strategy PnL before the first allocation anchor cannot be recovered from an
account-level snapshot: that snapshot does not record strategy ownership. To
begin an auditable strategy PnL period, capture the complete Exec allocation as
one immutable snapshot. The command reads the configured loopback Exec Viz
origin, requires the factual position state to be ready, and records the
snapshot's own timestamp and mark values:

```bash
nav_strategy_snapshot \
  --config /home/el01/crypto_cta_manager/config/cta-manager.toml \
  --source binance_exec_trade01 \
  --from-exec-viz \
  --venue-code 1 \
  --dry-run
```

After reviewing the printed snapshot, run the same command without `--dry-run`
to insert the immutable anchor:

```bash
nav_strategy_snapshot \
  --config /home/el01/crypto_cta_manager/config/cta-manager.toml \
  --source binance_exec_trade01 \
  --from-exec-viz \
  --venue-code 1 \
  --note 'strategy PnL allocation rebase'
```

`SYSTEM_POSITION_CLOSE` and any other non-strategy remainder are never
invented into a CTA strategy. The command records their account reconciliation
quantity as `__unallocated__`. After the anchor, `__unallocated__` is the
derived residual: for each symbol and venue it owns the account position and
NAV that the named strategy ledgers do not explain, matching how Exec assigns
the same remainder to `SYSTEM_POSITION_CLOSE`. System-close fills stay
unattributed, so they move the residual directly instead of consuming named
strategy lots, and the strategy rows always sum back to the account NAV. The
account NAV and the sum of strategy NAV therefore use the same anchor. Earlier
account history is excluded from the rebased strategy NAV rather than being
assigned without evidence.

For a controlled manual import, provide every nonzero strategy lot with a
single common mark for each symbol and venue:

```bash
nav_strategy_snapshot \
  --config config/cta-manager.toml \
  --source binance_exec_trade01 \
  --snapshot-ts-us 1787636000000000 \
  --position CTA_ALPHA:BTCUSDT:1:0.01:80500 \
  --position CTA_BETA:BTCUSDT:1:-0.002:80500
```

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

The timeline can display the selected account as a portfolio, by symbol, or by
the strategy suffix in either `batch_exec:<strategy_name>` or
`chase_exec:<strategy_name>`. Before an allocation
anchor, account-level initial snapshot positions remain unallocated because they
do not contain historical strategy ownership. Once an immutable strategy
allocation anchor exists, it replaces the older account anchor for that source.

The portfolio view also overlays theoretical NAV before and after estimated
fees. A background materializer consumes archived position updates by cursor,
freezes each nonzero binding-level delta when the complete scaled target vector
changes, and prices that delta as five equal-quantity virtual fills. The five
prices are 5-second mid bars sampled 60 seconds apart, beginning with the first
complete 5-second bar after the target update. Their arithmetic mean is stored
as the synthetic fill price and is equivalent to summing the five equal-quantity
costs. Later target changes own independent schedules and never truncate earlier
deltas. Each account has an editable theoretical TWAP fee rate, initialized to
the average of its Maker and Taker rates. The rate is frozen when the update is
staged and stored with the synthetic fill, so later fee edits do not rewrite
history. PostgreSQL stores only pending work, current target/FIFO
state, skips, and one sparse event per nonzero synthetic symbol fill; it does
not copy the 5-second BBO archive. Repeated publications of an unchanged
account/binding target advance the archive cursor without adding pending work
or changing an existing delta schedule. While a source has a nonzero
theoretical position, one source-level portfolio mark is materialized at most
every five minutes from the latest completed 5-second mid; empty periods
produce no mark rows. Pending rows and closed FIFO lots are deleted as they are
consumed. The first run backfills only the configured TWAP retention window,
currently 30 days.

The virtual acquisition cost for one symbol is
`delta_qty / 5 * sum(sample_mid[0..5])`; its estimated fee is
`abs(delta_qty) / 5 * sum(sample_mid[0..5]) * theoretical_twap_fee_rate`.
These execution-cost values do not require a mark price. FIFO and periodic marks
are retained only to render the optional theoretical NAV overlay.

`GET /api/catalog/acquisition-cost` is the direct actual-versus-virtual ledger.
For each materialized delta it returns the five source mids, virtual VWAP,
virtual turnover and fee, then maps each same-direction factual strategy fill by
its order `signal_ts_us` to the latest preceding symbol delta. The comparison
keeps the factual fill quantity unchanged and replaces only its price and fee;
target completion is reported separately from price shortfall. The aggregate
reports actual-fill reference coverage, actual Maker/Taker fee, virtual blended
fee, price shortfall, fee shortfall, and their sum. Symbol, side, liquidity-role,
target-to-fill delay, and order-signal-to-fill delay breakdowns add exactly to
the comparable total. Positive shortfall means factual acquisition was more
expensive. This endpoint never reads a mark price. Its browser is
`/manager/acquisition-cost/`.

The JSON returned by `GET /api/timeline` and `GET /api/account-timeline`
contains the portfolio-only series under `theoretical`. Query-time work is a
direct read of the last pre-window portfolio point plus stored points in the
requested window. Account filters apply, but no theoretical per-symbol or
per-strategy curves are exposed. The theoretical overlay is therefore hidden
when the browser selects only part of the symbol universe. Like the factual
timeline, returned values are rebased to zero at the selected window start.

`GET /api/timeline` is the strategy-attribution timeline and uses the latest
strategy allocation anchor when one exists. `GET /api/account-timeline` uses
only the account-level PostgreSQL position snapshot and later RocksDB fills; it
therefore remains available for total-account PnL before a strategy allocation
anchor. The browser exposes these as separate `策略归因` and `账户 PnL` modes.

For DataFrame clients, `GET /api/pnl/account` returns a versioned Arrow IPC
account-PnL table keyed by `ts_us`; `GET /api/pnl/strategies` returns a long
Arrow IPC table keyed by `strategy_name, ts_us`. Both accept `startMs`, `endMs`,
`sourceIds`, `symbols`, and `maxPoints`. Both include independent
`realized_pnl_before_fee_quote`, `floating_pnl_quote`,
`estimated_trading_fee_quote`, `nav_change_before_fee_quote`, and
`nav_change_after_fee_quote` columns. The Python SDK is available as
`scripts/manager_pnl_sdk.py` and at `GET /api/manager_sdk.py`.

`GET /api/nav/exchange` is a parameter-free real-time JSON endpoint separate
from PnL reconstruction. It returns the latest account-monitor exchange wallet
push for every enabled source, including
`equity_usdt`, wallet balance, exchange unrealized PnL, available balance,
exchange timestamp, and freshness status. It is an in-memory latest snapshot,
not a historical equity series and not part of the FIFO PnL totals.

## Strategy PnL Arrow Export

`GET /api/pnl/strategy` returns raw PnL rows for exactly one enabled account
and one `batch_exec:<strategy_name>` strategy. It requires camel-case query
parameters `sourceId`, `strategyName`, `startMs`, and `endMs`; timestamps are
Unix milliseconds and both interval boundaries are inclusive. The response is
an Arrow IPC stream with Zstd-compressed record batches and content type
`application/vnd.apache.arrow.stream`.

Rows are not resampled. `window_start` is a zero PnL baseline at `startMs`,
each `fill` row is one fill attributed to the requested strategy, and
`window_end` is the exact final PnL at `endMs`. Fills before the window build
the isolated `source_id + strategy + symbol + venue` FIFO state but are not
returned. The terminal row uses the latest account-level fill price for each
symbol and venue, so its floating PnL remains additive to the account even if
another strategy supplied the last mark.

The stream has identifiers, timestamp, row kind, optional fill identity, and
the cumulative window PnL fields `realized_pnl_before_fee_quote`,
`estimated_trading_fee_quote`, `realized_pnl_after_fee_quote`,
`floating_pnl_quote`, `nav_change_before_fee_quote`, and
`nav_change_after_fee_quote`. Monetary values are Arrow `Float64`, preserving
the existing Rust `f64` result bit-for-bit; the endpoint does not round or
reduce precision.

`GET /api/pnl/strategy/summary` accepts the same query parameters and returns
only the final selected-window totals as JSON. It does not return raw fill rows
and does not require an Arrow client.

```python
import pyarrow as pa
import pyarrow.ipc as ipc
import requests

response = requests.get(
    "http://172.16.30.42:10041/manager/api/pnl/strategy",
    params={
        "sourceId": "binance_exec_trade01",
        "strategyName": "CTA_ALPHA",
        "startMs": 1755648000000,
        "endMs": 1755734400000,
    },
    timeout=60,
)
response.raise_for_status()
table = ipc.open_stream(pa.BufferReader(response.content)).read_all()
frame = table.to_pandas()
```

The checked-in client validates the Arrow metadata and window rows. Its shortest
form queries `el01` / `binance_exec_trade01` for the previous day and prints a
JSON summary:

```bash
python3 scripts/manager_pnl_client.py CTA_SK_C4V6PosT1_LXY_filter_Position
```

Use options only when selecting another account, window, host, or Arrow output:

```bash
python3 -m pip install pyarrow
python3 scripts/manager_pnl_client.py \
  CTA_SK_C4V6PosT1_LXY_filter_Position \
  --target jp-meta \
  --source-id binance_exec_trade01 \
  --days 7 \
  --output /tmp/trade01-cta-pnl.arrow
```

```python
import pyarrow as pa
import pyarrow.ipc as ipc

table = ipc.open_stream(pa.memory_map("/tmp/trade01-cta-pnl.arrow", "r")).read_all()
frame = table.to_pandas()
```

Order strategies include `max_batch`, the estimated maximum batch count for a
single target update. When a target is activated, Exec values the outstanding
base quantity with the current mark price and calculates
`dynamic_single_usdt = delta_usdt / max_batch / orders_per_batch`. The effective
single-order amount is `max(single_order_usdt, dynamic_single_usdt)` and remains
fixed for that target generation. The Manager form shows the corresponding
maximum maker-path estimate:
`(max_batch - 1) * batch_interval_ms + (max_maker_requotes + 1) * maker_timeout_ms`.
Chase uses its own sizing contract. It freezes
`effective_batch_usdt = max(batch_floor_usdt, initial_delta_usdt / max_batch)`
for each target generation, releases one level-0 child per pass, and keeps the
total unfilled water level within
`effective_batch_usdt * max_open_batches`. Partial fills reopen the same amount
of capacity; the final residual may be smaller than `batch_floor_usdt`.
Chase also accepts `strategy_order_rate_limit_per_min` and
`strategy_order_rate_limit_10s` (both default to `0`, disabled). These rolling windows
aggregate new orders and amendments across every symbol under the actual
runtime strategy name. They are cumulative with the account Exec limits;
cancels consume neither quota. Strategy limits are top-level parameters and
cannot be selected through a symbol override.
An account binding's order strategy is the default execution template. A position
strategy may provide `symbol_order_strategy_overrides`, keyed by uppercase symbol
and valued by another named order-strategy template. Manager resolves those templates
at publish time and sends the effective per-symbol parameter differences to Exec.
Overrides affect execution parameters only, not target qty, binding shares, target
signal, or exchange contract leverage.

Build the API and frontend locally, then deploy one independent host:

```bash
# compile here, then upload to one physical host
scripts/deploy_host.sh --target el01
scripts/deploy_host.sh --target jp-meta

# or compile first and reuse the artifacts
cargo build --release --bin cta_web --bin nav_rebuild --bin nav_snapshot --bin nav_strategy_snapshot
cd frontend && npm install && npm run lint && npm run build
../scripts/deploy_host.sh --target el01 --skip-build
```

`el01` and `jp-meta` are two physical machines with two independent
stacks. The same local artifacts can be copied to either host. They do
not share PostgreSQL, Redis, Nginx, Manager RocksDB, or Exec accounts.
The live host `cta-manager.toml` is never overwritten; a
`cta-manager.toml.template` is uploaded beside it.

The known Exec deployment uses these loopback-only endpoints:

```text
CTA API:       127.0.0.1:18201
User Nginx:    127.0.0.1:10051
Manager:       /manager/
Manager Config:/manager/config/
Manager API:   /manager/api/
Exec Viz:      /exec_trade01/ through /exec_trade04/ (reserved: /exec_trade05/)
Exec Config:   /exec_trade01/config/
```

Nginx is installed without `sudo` by `scripts/install_nginx_user.sh`. The
script extracts pinned Ubuntu Noble packages under `~/.local/opt/nginx`, uses
the Tencent Ubuntu mirror by default, and verifies both package hashes before
installing. Host-global Manager, frontend, and Nginx files live under
`deploy/crypto_cta_manager/` and are installed to
`/home/el01/crypto_cta_manager` on the Exec host. They must not live under an
Exec account directory such as `binance_exec_trade01`.

jp-meta uses `deploy/jp_meta/` and installs to `/home/ubuntu/crypto_cta_manager`.
That host already has system PostgreSQL, Redis, and Nginx on port `4191`;
Manager publish is added as `/manager/` on that existing gateway instead of a
second user Nginx. `trade01` is enabled for catalog/publish against the
reserved Exec Config on `127.0.0.1:18161`. `trade02`/`trade03`/`trade04`
stay reserved and disabled. Do not regenerate the whole 4191 site from
`nginx_locations.txt`; install the CTA snippet instead.

Keep the service loopback-only like the existing Exec Viz deployment. Access it
through an SSH tunnel:

```bash
ssh -N -L 10051:127.0.0.1:10051 cta_exec
```

Then open `http://127.0.0.1:10051/manager/`. The same Nginx entry point also
proxies the Exec dashboard, WebSocket, snapshots, and configuration service
below `/exec_trade01/`. On el01, `trade01` through `trade04` are deployed and
enabled. `trade05` has reserved `/exec_trade05/`, `10045`, and `18165` routes,
but stays disabled and stopped until explicitly activated.

GET and POST from browsers and scripts must go through that environment's
Nginx. Do not call loopback `18201` or `18161` from outside the host.

el01 is reached from `el_dev` at `http://172.16.30.42:10041`. The user service
forwards that address to the Exec host's loopback Nginx on port `10051`; the
SSH destination is stored only in
`~/.config/crypto-cta-manager/gateway-tunnel.env` on `el_dev`. jp-meta is
reached directly at `http://13.115.227.29:4191`. The two hosts do not share an
IP or port.

```text
el01     http://172.16.30.42:10041/manager/
el01     http://172.16.30.42:10041/manager/workspace/
el01     http://172.16.30.42:10041/manager/api/
el01     http://172.16.30.42:10041/exec_trade01/
el01     http://172.16.30.42:10041/exec_trade01/config/
jp-meta  http://13.115.227.29:4191/manager/
jp-meta  http://13.115.227.29:4191/manager/workspace/
jp-meta  http://13.115.227.29:4191/manager/api/
jp-meta  http://13.115.227.29:4191/exec_trade01/
jp-meta  http://13.115.227.29:4191/exec_trade01/config/
```

On el01, `/` redirects into `/manager/workspace/`. On jp-meta, `/` stays the
host Nginx welcome page; the CTA workspace is `/manager/workspace/`.
`/manager/` is the NAV timeline. `/manager/config/` owns strategy catalog,
account bindings, and publish. The Exec `/exec_trade01/config/` page stays
read-only. Runtime Redis JSON is written only by Manager through the loopback
Exec Config `POST /api/strategy`. There is no write token. Each target is
`{qty, signal}`; `signal=±1` means that symbol uses taker-only for the current
execution. A successful `POST /api/catalog/position-strategies` republishes
every active bound account automatically using `qty × shares`. Each binding
stores a non-negative `shares` multiplier. Setting it to zero immediately
publishes one complete zero target vector under the original strategy name,
then excludes that binding from later automatic publishes. This closes the
strategy without routing its fills through `SYSTEM_POSITION_CLOSE`. Manager
keeps a reconnecting Redis long connection, writes and rereads the runtime JSON there, then notifies
`exec-pre-trade` over iceoryx. The 30s Redis poll remains the fallback.

Before publishing any non-zero target, Manager checks that source's live account
mode with its Exec `env.sh`. A Binance USD-M source must use Standard API mode
with Multi-Assets Mode enabled (`/fapi/v1/accountConfig`). An OKX source must be
a unified account: `acctLv` 3 (multi-currency margin) or 4 (portfolio margin)
from `/api/v5/account/config`. Query failures also block the publish. Complete
zero target vectors bypass this gate so an operator can always stop a strategy
and reduce risk. Live Maker/Taker queries use Binance `commissionRate` or OKX
`/api/v5/account/trade-fee`. OKX selects the instrument `groupId` inside
`feeGroup`; a negative OKX rate is a cost and is returned with the positive
Manager sign.

Exchange contract leverage is an independent venue margin setting. Query and
set it per account and per symbol through Manager. Both calls read that account's
Exec `env.sh` (default `<rocksdb_path>/../../env.sh`). GET is the live venue
value; PUT records the last requested value in PostgreSQL as
`recorded_contract_leverage`. Neither call scales published qty, writes Exec
Redis, or notifies `exec-pre-trade`. Range is 1–125.

Account configure users may read and update the shared Exec order-action rate
limits through
`GET/PUT /api/catalog/accounts/{source_id}/exec-order-rate-limits`. Manager
updates only `exec_order_rate_limit_per_min` and
`exec_order_rate_limit_10s` in that account's
`{source_id}:{venue}:pre_trade_risk_params` Redis hash and immediately reads
them back. It does not replace the other risk fields. `0` disables the
corresponding window. Batch, POV, and Chase new orders plus Chase amendments
share this account-local Exec bucket; cancels are not counted. Exec picks up a
saved value on its existing risk-parameter refresh, within about 60 seconds.

```bash
# el01
curl --noproxy '*' -sS \
  'http://172.16.30.42:10041/manager/api/catalog/accounts/binance_exec_trade01/contract-leverage?symbol=BTCUSDT'

curl --noproxy '*' -sS -X PUT \
  'http://172.16.30.42:10041/manager/api/catalog/accounts/binance_exec_trade01/contract-leverage' \
  -H 'Content-Type: application/json' \
  -d '{"symbol":"BTCUSDT","contract_leverage":5}'

# jp-meta is a different physical host. Choose it explicitly.
python3 manager_publish_client.py --target jp-meta get-contract-leverage binance_exec_trade01 BTCUSDT
python3 manager_publish_client.py --target jp-meta set-contract-leverage binance_exec_trade01 BTCUSDT 5
python3 manager_publish_client.py --target jp-meta get-execution-cost --window-sec 300
python3 manager_publish_client.py --target el01 get-contract-leverage binance_exec_trade01 BTCUSDT
```

External push scripts only POST the catalog
through the same Nginx:

```text
el01     http://172.16.30.42:10041/manager/api/manager_publish_client.py
jp-meta  http://13.115.227.29:4191/manager/api/manager_publish_client.py
```

Do not POST targets or full configs to Exec Config.
