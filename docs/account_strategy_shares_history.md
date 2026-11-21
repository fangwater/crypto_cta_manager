# Actual account strategy shares

The history dimension is **actual account (`source_id`) × underlying position
strategy (`strategy_name`)**. Publication binding names, order templates and
Virtual portfolios are not separate allocation identities. The account overview
and strategy configuration pages show current effective shares and changes.
Account view permission is sufficient to read the overview and history API.

## Virtual expansion and totals

Use the existing Virtual DAG expansion and materialized actual-account bindings:
multiply leaf shares by every child portfolio ratio and the account follow
multiplier, then sum all contributions to the same actual account and leaf
strategy, including contributions with different publication aliases.

For example, Virtual P contributes A × 2 and Virtual Q contributes A × 3 after
all multipliers. The account displays one **A × 5** row. Changing P/Q while
keeping the final A total at 5 does not create a history change. An unfollowed
Virtual edit creates no actual-account history. Nested Virtuals use exactly the
same resolved bindings as normal publication; the history does not implement a
second graph or infer shares from factual execution quantities.

Different execution sources retain their identities. A logical multi-market
account displays its sources with market labels; it never adds duplicate shares
across venue legs. Changing only an execution template, a publication alias, or
follow mode while preserving the leaf allocation is not a shares change.

## One-minute consolidation

Consecutive changes to the same actual account and underlying strategy with a
gap of **at most 60 seconds** update one existing row. The window rolls from the
last actual total change. Preserve the initial total/time and update the final
total/time, count, participating operators and origins. A round ending at its
initial shares is hidden, with one stored row retained so a further change can
resume the same rolling window even after restart. Missing allocation means
zero; an initial allocation is `0 → shares`, and removal is `shares → 0`.

## Atomic configuration snapshots

Each configuration transaction captures the account's strategy totals before
mutation and compares them to the totals after **all** of its binding updates.
Virtual materialization records only the final totals, so moving contributions
between aliases cannot create intermediate false changes or extend the merge
window. Stable binding order keeps floating-point summation deterministic.
Invalid or overflowing totals fail the configuration transaction.

Manual saves share the existing PostgreSQL configuration advisory lock used by
Virtual changes. The allocation mutation and its compact history both commit or
roll back together. This requires no pending in-memory minute timer. Operators
and origins are passed explicitly within the transaction. Direct out-of-band
SQL maintenance is not an audited configuration API; it must not be used to
claim complete change history.

`cta_account_strategy_shares_history` is independent of the 33-day position
archive. It has no cascading catalog foreign keys. Times are configuration-save
times, not confirmed Exec receipt times. Normal target publication, Redis/IPC,
Exec stores and cold pruning are unchanged.

## Installation and read API

After a recoverable PostgreSQL backup, explicitly apply
`scripts/apply_account_strategy_shares_history_schema.sql` before deploying the
matching Manager binary/frontend. The additive maintenance creates only the
history table and indexes; it preserves all existing allocations, snapshots and
targets. New databases use `cta_web --init-db`; normal startup never runs DDL.
On 2026-11-18, jp-meta already ran the matching Manager binary, and the
missing table blocked binding changes. A PostgreSQL backup passed a full
`pg_restore` read before the maintenance ran. The first 16 entries record the
Spread01 and Spread02 bindings changing from 0 to 1 on OKX and Binance.
The backup and activation receipts are in the Manager host's
`maintenance-backups/cta_cross_resume_20261118/` directory.

The current allocation is immediately readable from existing materialized
bindings. History starts with the new code's first actual allocation change;
there is no automatic backfill or invented historical save time.

`GET /api/catalog/accounts/{source_id}/shares-history` returns `current`,
`entries` and `next_cursor` from a consistent read-only SQL snapshot. Optional
parameters: `strategy_name`, `limit` (1–200, default 50), and paired
`before_at_us` / `before_id` cursor. History is newest last-change time, then
change ID. Both current allocations and changes use only the account's allowed
execution sources. The UI refreshes after local saves and supports manual
refresh and loading older records.

`tests/postgres_schema/shares_history.rs` covers the rolling minute, reversions,
concurrent saves, atomic rollback/storage errors, alias aggregation, nested
Virtuals, account multipliers, offsetting contributions, inactive Virtuals,
source isolation, stable cursor ties, and explicit schema maintenance.
