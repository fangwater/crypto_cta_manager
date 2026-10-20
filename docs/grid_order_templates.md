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
