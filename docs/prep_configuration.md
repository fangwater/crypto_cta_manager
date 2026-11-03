# Prep configuration navigation

Last updated: 2026-11-03 UTC.

The Manager account card, account overview and strategy binding cards expose a
“信号配置” link for accounts with configure access. Account links select 1min;
each strategy link selects its own frequency.

| Stable Exec source | Prep group | Frequencies |
| --- | --- | --- |
| `binance_exec_rx01` | `prep01` | 1min, 5min, 15min |
| `binance_exec_rx03` | `prep02` | 1min, 5min, 15min |

Links use the current gateway origin and the shared editor at
`/cta/cta-prep-binance-futures/config/?worker=cta-prep-binance-futures`.
Prep01 hash names are `binance-futures_<frequency>`; Prep02 hash names are
`prep02_binance-futures_<frequency>`. Account labels and retired gateway slots
must not determine the destination.

The shared editor also manages Spread. Its Nginx authorization scope is
`binance_exec_rx01,binance_exec_rx03,okex_exec_trade01,binance_exec_spread01`,
requiring configure access to every affected account. The retired
`binance_exec_trade06` must not remain in that scope.

JP static release `prep_config_links_20261103T103355Z` deployed UI commit
`5d70422024a486b9155688e4a5320ac0e0d5f208`. The frontend build and browser checks
passed for all six frequency links, account and binding buttons, mobile
visibility and hidden-user isolation. The gateway scope was corrected after
the browser check exposed an existing 403. Nginx syntax validation and graceful
reload passed; Manager, calculation and trading processes remained running.
No strategy settings or targets were changed.

Local receipts are under the Git-ignored
`artifacts/prep_config_links_20261103/`. Private remote rollback records are
under `~/crypto_cta_manager/maintenance-backups/prep_config_links_20261103T103355Z/`
and the matching `_gateway` directory. Upload staging and the local archive
were removed after successful verification.
