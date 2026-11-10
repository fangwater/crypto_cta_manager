# Prep and Spread configuration navigation

Last updated: 2026-11-10 UTC.

The Manager account card, account overview and strategy binding cards expose a
“信号配置” link for accounts with configure access. Account links select 1min;
each strategy link selects its own frequency.

| Stable Exec source | Prep group | Frequencies |
| --- | --- | --- |
| `binance_exec_rx01` | `prep01` | 1min, 5min, 15min |
| `binance_exec_rx03` | `prep02` | 1min, 5min, 15min |
| `cta_cross_spread_rx01` (combined account `okex_binace_cross`) | `spread01` | 1min, 5min, 15min, 1h |

Links use the current gateway origin and the shared editor at
`/cta/cta-prep-binance-futures/config/?worker=cta-prep-binance-futures`.
Prep01 hash names are `binance-futures_<frequency>`; Prep02 hash names are
`prep02_binance-futures_<frequency>`. Account labels and retired gateway slots
must not determine the destination.

Spread uses the same editor with `?worker=cta-factor-spread` and hash
`okex-futures_binance-futures_<frequency>`. Its account button opens 1min;
the four `cta-spread01-okx-bn-<frequency>-pos` strategies open their own tabs.
Navigation uses the public combined account identity on both execution-market
views; neither execution source becomes a separate public account. There are
no Spread symbol pools. The shared helper is
`frontend/src/lib/signalConfigLinks.ts`.

The shared editor also manages Spread. Its Nginx authorization scope is
`binance_exec_rx01,binance_exec_rx03,okex_exec_trade01,binance_exec_spread01`,
requiring configure access to every affected account. The retired
`binance_exec_trade06` must not remain in that scope.
The editor uses the same Manager session as the account workspace. It requires
configure access to every account above, including on catalog reads; a separate
Delist login is unnecessary. Non-GET/HEAD requests require an explicit Origin
matching the gateway origin. Nginx overwrites the audit actor as
`manager_session`; the configuration journal retains the session fingerprint.
The status-only Manager verifier does not supply a username, so this marker
must not be presented as an identified human author.

On 2026-11-10, gateway release `cta_factor_config_auth_20261110T043440Z`
corrected the additional Delist gate that caused HTTP 401 after account-page
navigation. Real browser checks with a Manager-only session loaded all four
Spread tabs with 28/56/18/107 rules and read both worker catalogs with HTTP 200.
Anonymous access remained HTTP 401; an account without configure access and
missing/cross-origin writes were rejected with HTTP 403. Only the Nginx snippet
was replaced and gracefully reloaded. Manager, workers and trading processes
were not restarted; strategy files remained unchanged. Receipts are in the
mkt_signal repository's `artifacts/cta_factor_config_auth_20261110/`.

JP static release `spread_signal_config_links_20261110T042118Z` adds Spread
navigation to the account card, account overview and strategy binding cards.
It changes static assets only. The frontend build, 14 focused account tests,
mapping assertions and desktop/mobile browser checks passed. Browser checks
cover both execution-market selections, Prep regression links, hidden-account
isolation and all four exact Spread destinations. They used a temporary
Manager session and verified the existing Delist login requirement; they did
not authenticate or save the editor. Receipts are in
`artifacts/spread_signal_config_links_20261110/`.

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
