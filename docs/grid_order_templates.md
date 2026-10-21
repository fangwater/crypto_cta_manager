# Grid order templates

Manager supports Grid templates for RapidX and supported native unified accounts.
The form has one shared single-order size and the same controls for each market:

| Field | Spot default | Futures default |
| --- | --- | --- |
| `vol_multiplier` | 1.0 | 1.0 |
| `orders_per_batch` | 3 | 3 |
| `maker_timeout_ms` | 1000 | 1000 |
| `max_maker_requotes` | 2 (3 total maker rounds) | `null` (unlimited) |

Each market independently supports finite maker rounds followed by one taker for
the remaining quantity, or unlimited maker requotes. The UI counts the initial
maker round: N rounds is stored as `max_maker_requotes = N - 1`; `null` is the
only unlimited value. A limit of zero means one maker round. Finite limits must
be below uint32 max. Switching to taker waits for all previous orders' terminal
reports and deducts partial fills. Data-staleness pauses do not consume rounds.
The existing signal execution switch still allows explicit taker instructions.

Multipliers must be finite and positive. Each side's offsets run from zero to
`vol * vol_multiplier`, evenly spaced across its market's split count; one
level has zero offset. Raw volatility and freshness rules are unchanged. The
execution parameters depend only on the target's market: spot/perpetual pairs
and standalone perpetual strategies both use Grid without spread membership
checks. Each market still requires its own ready volatility feed.

New order templates belong to the logged-in creator. The catalog returns
`owner` and session-specific `can_configure`; only the owner or an administrator
can save or delete an owned template. Existing templates without a creator stay
shared. Ownership does not change template visibility or account binding rights.
Renaming in the editor creates a new template owned by the current user.

For an existing Manager database, explicitly apply
`scripts/apply_grid_order_schema.sql` and
`scripts/apply_order_strategy_ownership.sql` before installing the new Manager.
Databases with the initial draft Grid JSON must explicitly apply
`scripts/apply_grid_market_parameters.sql`. Run the same explicit maintenance
for existing per-market Grid JSON before installing these controls: it adds
missing multipliers as 1.0 and missing futures limits as `null`, preserving
existing split counts, durations, spot limits, and any already-set controls.
It is idempotent and leaves catalog timestamps and other parameters unchanged. Fresh databases use `migrations/schema.sql`; startup never
performs DDL.

Grid parameters are emitted to Exec only when the selected algorithm is Grid.
A Grid symbol override includes its complete Grid settings when its base uses a
different algorithm. Saving an unbound template only updates the Manager catalog;
activating it requires a compatible Exec and its volatility producer.

## Local change, 2026-10-21

Manager, Exec and Exec Config now share the fields above. Deploy the matching
artifacts together; the older Exec rejects the new fields. This framework change
does not publish targets, change RX01 bindings or activate live Grid trading.

Validation: Manager full crate tests (377 library and 9 binary), 15 isolated
PostgreSQL tests, frontend production build, Exec library tests (1,338 passed,
3 ignored), and 47 Exec Config Python tests. Coverage includes independent
multipliers, finite/unlimited rounds in either market, terminal-confirmed taker
fallback after partial fills, symbol overrides, and idempotent catalog maintenance.

## Japan deployment (2026-10-20, before these controls)

Updated 2026-10-20 UTC. Japan Manager release `20261020T131400ZGridMarkets`
uses code commit `73a27c4` and binary SHA-256
`9d3201bda97b68b44f4ab5a9907abac157189dcfdf79da0860d923c5c311c93d`.
The `lgq_grid` template belongs to `lgq` and remains unbound. Its original
single-order size, split count and maker duration were retained as the starting
values for the two independent market configurations.

Browser verification through the Japan Nginx gateway saved and reloaded distinct
spot/futures split counts (2/5) and maker durations (2100/9000 ms) on a temporary
lgq-owned template, then deleted it. The original template was unchanged.
Desktop and mobile rendering passed; volatility controls and the futures maker
retry input are absent. Unsupported volatility fields and futures retry limits
are rejected by the API. Existing 12 templates' other fields, 87 account bindings,
97 trading/gateway process identities, host configuration and Manager CPU 24
were preserved. Only Manager was restarted; Exec/factor deployment remains pending.

Validation: 365 Manager library tests and 9 binary tests, 14 isolated PostgreSQL
tests, frontend production build; 1,336 Exec library tests and 46 Python Config
tests. Manager runtime serialization preserves the two market settings; Exec
selects them by market and ignores edits to the inactive market while orders
remain working.
