# OKX / Binance Spread

Manager supports one position strategy containing venue-qualified paired USDT
perpetual targets. For example `okex-futures:ETHUSDT` +2 and
`binance-futures:ETHUSDT` -2. Bind that position strategy to an existing OKX
unified Exec and an independent Binance USD-M Exec with matching shares.
Each binding receives only its venue's ordinary `ETHUSDT` target. The OKX account
stays unified; no account-mode mutation is introduced. A directional entry may
have a nonzero OKX target and an explicit zero Binance target. Removing a symbol
requires explicit paired clear targets before omitting it, as with other strategies.

Both market keys are required per asset, including zero and clear targets.
Do not mix these keys with ordinary targets in one strategy. Other exchanges and
Bybit target proxies cannot bind this mode. Catalog JSON uses the existing tables;
no database maintenance or new internal API format is required. Account projections
and effective shares are frozen in the existing position archive before publish.
Each Exec keeps its existing market ownership and receives normal unqualified symbols.
Execution and delivery across the two accounts are independent and retryable, not atomic.

The existing target editor and publish client accept these target names. Parameter
overrides use the same qualified keys and are projected to the matching account.
Native OKX still owns its separate cash market, but these targets only address
perpetuals. Account NAV selection and strategy permissions keep their existing scope.

OKX minute candles are fetched lazily through `/api/v5/market/history-candles`
with the existing verified dedicated Kline egress and shared concurrency/budget.
Only confirmed closed minutes are accepted. Historical requests are bounded to
300 rows; the cache retains the existing 56-byte value codec and 30-day retention.
OKX swap base volume comes from `volCcy`, quote volume from `volCcyQuote`, never
from the contract-count `vol`. This API does not supply a trade count; the cache's
unused count slot is zero for OKX. Cache keys use `okex-futures:<symbol>` or
`okex-margin:<asset>` so prices never collide with Binance. All factual minute
marks, theoretical schedules and acquisition-cost references use that market key.
Uncovered prices remain unavailable and continue lazy backfill.

Public Funding uses OKX `/api/v5/public/funding-rate-history`, paginated backwards,
with the realized rate when supplied. It does not collect signed account income.
The public history lacks settlement mark prices: the estimate uses the immediately
preceding confirmed 1m close from `/api/v5/market/history-mark-price-candles`.
Its `rate_type` is `OKX_closed_1m_mark_estimate`, explicitly distinguishing that
reference from an exact exchange settlement mark. Missing mark minutes are unknown;
those settlements are excluded individually and the result reports partial coverage.
The cache retries missing marks and reuses already loaded valid mark references.
Default refresh symbols remain Binance-only; OKX is requested lazily by NAV queries.
Requests for history older than OKX's three-month source limit remain unavailable.

Estimated funding replays the same source/venue-isolated snapshots, factual fills,
strategy ownership and allocation corrections as FIFO NAV. OKX and Binance rates
cannot price each other's holdings. Account totals and strategy subtotals remain
additive, including unallocated positions. Cumulative Funding cards and strict
minute-priced daily equity reconstruction also accept OKX linear sources.
The existing actual-versus-estimated
funding selection is preserved. No live deployment or source binding was performed.

Authoritative contracts:
https://www.okx.com/docs-v5/en/#rest-api-market-data-get-candlesticks-history
https://www.okx.com/docs-v5/en/#rest-api-market-data-get-mark-price-candlesticks-history
https://www.okx.com/docs-v5/en/#rest-api-public-data-get-funding-rate-history
