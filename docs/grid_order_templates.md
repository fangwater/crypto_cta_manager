# Grid order templates

Manager supports Grid templates for RapidX and supported native unified accounts.
The form has one shared single-order size, independent spot/futures split counts
and maker durations, and a spot-only maker round count. Futures maker rounds
are unlimited. Volatility grid calculation and data freshness are algorithm
internals, not configurable template fields. The optional signal execution switch still
allows explicit taker instructions.

New order templates belong to the logged-in creator. The catalog returns
`owner` and session-specific `can_configure`; only the owner or an administrator
can save or delete an owned template. Existing templates without a creator stay
shared. Ownership does not change template visibility or account binding rights.
Renaming in the editor creates a new template owned by the current user.

For an existing Manager database, explicitly apply
`scripts/apply_grid_order_schema.sql` and
`scripts/apply_order_strategy_ownership.sql` before installing the new Manager.
Databases with the initial draft Grid JSON must explicitly apply
`scripts/apply_grid_market_parameters.sql`. This preserves existing split counts,
maker durations and spot retry limits as initial values for the independent
market settings. Fresh databases use `migrations/schema.sql`; startup never
performs DDL.

Grid parameters are emitted to Exec only when the selected algorithm is Grid.
A Grid symbol override includes its complete Grid settings when its base uses a
different algorithm. Saving an unbound template only updates the Manager catalog;
activating it requires a compatible Exec and its volatility producer.

## Japan deployment

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
