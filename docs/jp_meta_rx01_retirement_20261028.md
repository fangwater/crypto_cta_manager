# jp-meta RX01 retirement

On 2026-10-28, `binance_exec_trade06` / `lgq_cta_RX01` was stopped and
removed from the running Manager configuration. Its virtual03 follow was
removed, and all 65 account bindings were stopped at zero shares and deleted.
PostgreSQL catalog and historical snapshots were retained. Manager restarted
on CPU 24; authenticated health returned `ok` and the source disappeared from
the dashboard. The other configured sources were retained.

The Spread signal was stopped and its publication switch disabled. Exec
pre-trade, trade engine, account monitor, persist manager, Viz and Config were
stopped through their environment-local scripts. The Spread factor publisher
and signal UI support processes remain running. No trading IP was changed.

RapidX MARKET orders closed the account's cash and perpetual exposure. The
durable order journal contains 98 orders; 416 matching exchange execution
records reconciled to every order quantity and all reported `TAKER`. Final
exchange checks showed zero perpetual positions, debts, ordinary orders and
conditional orders. Fifty-five positive cash residuals were all below their
market quantity steps, worth about 3.80 USDT in total. The USDT balance and
available amount were 19007.330432544628896544; no transfer was requested after
the operator clarified that the funds should remain in RapidX.

The final archives are under
`/home/ubuntu/crypto_cta_manager/archives/rx01_retire_20261028/` on jp-meta:
`rx01_final_data.tar.gz` contains the closed Exec order store and RapidX account
event files; `rx01_final_evidence.tar.gz` contains the order journal, exchange
executions and reconciliation snapshots. Both archives passed gzip and
file-by-file comparison, with hashes recorded in `final_archives.sha256`.
Original data directories were moved to `original-data/` in the same private
archive directory. A separate pre-liquidation archive and a private Manager
database backup were also retained. The current Exec `data/` directory is empty.
