# NAV browser transport

The NAV browser requests the existing /api/timeline and /api/account-timeline
endpoints with Accept: application/x-protobuf. Existing JSON clients still get
the full JSON representation, and Arrow exports retain their existing contracts.
Source authorization, snapshot selection, FIFO, minute valuation, fees and
funding reconstruction run before projecting the browser response.

The current browser wire contract is src/web/timeline.proto. Small summary,
coverage and status objects use Protobuf Struct; time-series data never uses
embedded JSON. Curves reference shared exact-microsecond time axes and packed
double columns. A constant column sends one value. The seven factual chart
columns preserve the original plotted values, including both before-fee and
after-fee NAV, without changing floating-point arithmetic in the browser.
Execution counts and Maker/Taker/Unclassified volumes remain in interval and
strategy summaries rather than being repeated at every sample.

The view query selects the projection:

| View | Returned curves |
| --- | --- |
| portfolio | Account NAV/exposure and actual/estimated account funding |
| symbols | Portfolio plus nonempty symbol NAV/exposure curves |
| strategies | Portfolio plus strategy NAV and strategy funding curves |
| theoretical | Theoretical curves and execution slippage only |

Summaries remain available when their detail curves have not been requested.
Strategy activity is calculated before projecting away its samples, so held
positions, prior in-window activity, offsetting ownership and very small funding
remain visible. Only exactly zero symbol curves are omitted: a carried position
without fills, tiny nonzero holdings, rebates and opposing strategy funding are
not filtered by a notional or epsilon threshold. The source/symbol catalog and
all underlying history remain intact.

The factual request sets includeTheoretical=false. Once it completes, a
separate view=theoretical request supplies the overlay without retransmitting
the factual report or recalculating funding. Funding coverage and theoretical
coverage retry separately. An unavailable theoretical overlay leaves factual
NAV visible. Range and scope changes cancel pending requests.

Only Protobuf responses use the API's gzip middleware; target publication stays
outside that compression predicate. Manager Nginx configurations also enable
gzip for JSON, Protobuf and JS/CSS. Gzip is applied once: an already encoded
upstream response is passed through. Compression does not round chart values or
turn unknown funding into zero.

Build requires protoc; Rust generates its bindings with prost-build.
Regenerate the checked-in browser bindings after editing the schema:

    cd frontend
    npm run generate:nav-proto
    npm run build
    npm run test:nav-proto

The browser tests consume a synthetic fixture produced by the Rust encoder.
To regenerate it from the repository root:

    CTA_NAV_PROTO_FIXTURES="$PWD/frontend/tests/fixtures/nav-proto" cargo test --lib web::timeline_proto

Tests cover exact financial doubles and time stamps, shared axes, omitted empty
curves, carried dust, negative fees, null funding, invalid payloads, source
authorization, gzip HTTP round trips, and theory-only updates.
