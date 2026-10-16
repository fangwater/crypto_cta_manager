# Bybit / Bitget unified accounts

Manager supports native `bybit-futures` and `bitget-futures` sources for USDT
perpetual targets (`BTCUSDT`, `ETHUSDT`, etc.). Each source retains its own
account, IPC namespace, Redis prefix and persistence store. Bare assets, inverse
contracts, delivery contracts and USDC contracts are rejected. A unified wallet
does not expand the configured Exec market.

This is Manager support. Registering a source does not deploy an Exec, migrate
existing intra positions, repay borrowing or start trading. The Exec runtime
must support the same venue and Manager rule cache before enabling publication.

## Account data and credentials

`ipc_namespace` defaults to the source id. The default account IPC suffixes are
`account_pubs/bybit_pm` and `account_pubs/bitget_pm`. An explicit
`account_ipc_service` still takes precedence. The existing fixed binary account
event contract is unchanged: Bybit scope 13 and Bitget scope 12. Scope 14 (Bitget
inverse positions) and other exchanges' events cannot update these sources.

Manager reads exchange-valued **whole-account equity**, including non-USDT
collateral, from account-risk events. It does not estimate equity from the USDT
cash wallet. The API's existing `equity_usdt` field carries the monitor's USD
account valuation. There is no invented wallet/PnL breakdown. Risk coverage is
the already-normalized monitor ratio, with higher values safer: Bybit inverse
account MM rate; Bitget effective equity divided by maintenance margin.
Positive infinity (no maintenance margin) clears the displayed numerical ratio
to `--`; it does not retain an old finite ratio or fabricate a value. Timestamp
and stale-data handling remain active.

Private APIs read static assignments from the source's `env_path` (default
`<Exec>/env.sh`) and bind the first `local_ips` entry from the adjacent
`trade_engine.toml`, using the existing default-route resolver for `0.0.0.0`.
Missing IP configuration fails closed. Proxy and redirect following are disabled.

- Bybit: `BYBIT_API_KEY`, `BYBIT_API_SECRET`; optional `BYBIT_BASE_URL`.
- Bitget: `BITGET_API_KEY`, `BITGET_API_SECRET`, `BITGET_API_PASSPHRASE`;
  optional `BITGET_BASE_URL`.
- Native backend only. RapidX/LTP is rejected for these sources.

No credential values belong in Manager TOML, documentation or logs. Example
source definitions are in `config/cta-manager.example.toml` and remain disabled.

## Account controls

Nonzero-target publication checks the live account mode. Bybit must report UTA
status 3/4/5/6 and regular cross or portfolio margin. Bitget must report unified
or hybrid, basic/advanced/delta, and one-way positions. Missing fields, migration
states and query failures block nonzero publication. Existing all-zero/clear
publication semantics are unchanged.

Leverage queries/updates use Bybit `/v5/position/list` and
`/v5/position/set-leverage`, or Bitget `/api/v3/account/settings` and
`/api/v3/account/set-leverage`. Writes check position/margin configuration and
read back the resulting leverage. They do not change account, position or margin
mode. Bybit portfolio margin has no fixed per-symbol leverage and returns an
explicit error. Bitget cross leverage can be initialized for a previously
unconfigured symbol; a missing query value is never reported as a default.
The existing integer 1–125 Manager input range remains in force; exchange limits
still apply.

Fee queries use the authenticated `/v5/account/fee-rate` or
`/api/v3/account/fee-rate` endpoint for the requested symbol. Negative maker
rebates are preserved. Neither response supplies a VIP tier, so `vip_tier` is
null; existing Binance and OKX responses retain their numeric tier.

## Market rules and valuation

Manager refreshes one complete cache every 60 seconds. Bybit uses every cursor
page of `/v5/market/instruments-info?category=linear`; Bitget uses
`/api/v3/market/instruments?category=USDT-FUTURES`. Public calls use Manager's
configured public-data egress, independently of account API egress. The current
cache contract and Redis key are unchanged. The snapshot includes venue status;
Bybit `Trading` and Bitget `online` are the active statuses. A consumer must
disable omitted/inactive symbols. Duplicate symbols, repeated cursors, missing
fields or failed requests reject the refresh and retain the previous snapshot.
Bitget V3 multipliers are decimal increments, not precision exponents.

Factual fills/FIFO/NAV already support Bybit/Bitget venue codes and remain
venue-scoped. Binance-specific funding acquisition and minute-kline theoretical
NAV are not enabled for these venues; Manager must not use Binance candles or
funding as a substitute.

API references:

- [Bybit account information](https://bybit-exchange.github.io/docs/v5/account/account-info)
- [Bybit instruments](https://bybit-exchange.github.io/docs/v5/market/instrument)
- [Bybit positions](https://bybit-exchange.github.io/docs/v5/position)
- [Bybit leverage](https://bybit-exchange.github.io/docs/v5/position/leverage)
- [Bybit fees](https://bybit-exchange.github.io/docs/v5/account/fee-rate)
- [Bitget settings and leverage](https://www.bitget.com/docs/catalog/account/account-settings)
- [Bitget instruments](https://www.bitget.com/docs/catalog/market/market-data)
- [Bitget fees](https://www.bitget.com/docs/catalog/account/assets-balance#get-account-fee-rate)
