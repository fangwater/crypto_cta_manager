# RapidX Exec Rules

Manager remains the sole order-rule publisher. For each enabled source, the
existing `env_path` (or `<rocksdb_path>/../../env.sh`) supplies
`TRADE_ENGINE_EXEC_BACKEND` and `TRADE_ENGINE_EXEC_BACKEND_MAP`. Specific venue
mapping overrides `*`, which overrides the default. `rapidx` and `ltp` select the
same backend; published identity is `ltp`. Native venue names are unchanged.
No account-mode or additional source config field is introduced.

RapidX requires `LTP_API_KEY`, `LTP_API_SECRET`, and `LTP_PORTFOLIO_ID` in that
source env. The file is parsed, never shell-executed. Use literal assignments,
not shell substitutions or references to another variable. `LTP_REST_URL` may
override the API origin. Do not copy credential values into TOML or documentation.
An explicitly configured missing env file is an error. A source without a local
default env can still receive public native rules; RapidX Exec rejects their
native provenance until the source env is supplied.

Manager queries authenticated `GET /api/v1/trading/sym/info`. Only Binance and
OKX USDT perpetual markets enter this adapter. Precision steps and quantities
come from `tickSize`, `lotSize`, and `minSize`; `contractSize` is required.
OKX quantities stay in contracts, with the multiplier retained separately.
Binance quantities are base units and require a contract size of one. Zero
minimum notional becomes no minimum-notional filter. Suspended pairs are retained
but marked nontradable. Invalid target rows or an empty target snapshot fail the
whole refresh; there is no partial publication or native fallback.

The refresh interval stays 60 seconds. RapidX requests share four-second spacing
in the refresh worker, including failed attempts. Public native snapshots are
reused per venue only; RapidX snapshots are source-specific. Many RapidX sources
can make a refresh cycle longer than 60 seconds. Separate Manager processes or
other clients sharing credentials must coordinate their rate budgets.

The existing `<source_id>:<venue>:market_rules` JSON has one current format:
`venue`, `execution_backend`, `portfolio_id`, `fetched_at_us`, and `symbols`.
`portfolio_id` is null for native sources. There are no versioned keys or old
format readers. Coordinate replacement with the mkt_signal Exec consumer.
Exec checks backend/portfolio identity, and RapidX startup requires a validated
cache at most 180 seconds old before cancellation. Native and RapidX reloaders
retain their last good snapshot on subsequent refresh failure; a complete
snapshot that omits/suspends a symbol blocks new orders.

This change does not deploy, start processes, set leverage, or cancel live orders.
RapidX Exec's startup adapter handles scoped cancellation and leverage readback.
Manager's separate native account APIs refuse RapidX sources so they cannot
silently read or modify a different native account. Manager UI RapidX account
controls are not implemented by this rule-publishing change.

Protocol: [Symbol rules](https://apidocliquidity.readme.io/reference/sym-info)
and [REST authentication](https://apidocliquidity.readme.io/docs/authentication).
