# Grid order templates

Manager supports Grid templates for RapidX and supported native unified accounts.
The form configures shared single-order size, split count, maker duration and
spot maker round count, plus Grid volatility scales and minimum offset.
Futures maker rounds are unlimited. The optional signal execution switch still
allows explicit taker instructions.

New order templates belong to the logged-in creator. The catalog returns
`owner` and session-specific `can_configure`; only the owner or an administrator
can save or delete an owned template. Existing templates without a creator stay
shared. Ownership does not change template visibility or account binding rights.
Renaming in the editor creates a new template owned by the current user.

For an existing Manager database, explicitly apply
`scripts/apply_grid_order_schema.sql` and
`scripts/apply_order_strategy_ownership.sql` before installing the new Manager.
Fresh databases use `migrations/schema.sql`; startup never performs DDL.

Grid parameters are emitted to Exec only when the selected algorithm is Grid.
A Grid symbol override includes its complete Grid settings when its base uses a
different algorithm. Saving an unbound template only updates the Manager catalog;
activating it requires a compatible Exec and its volatility producer.
