CREATE TABLE cta_source_symbols (
    source_id text NOT NULL REFERENCES cta_order_sources(source_id),
    symbol text NOT NULL,
    venue_code smallint NOT NULL CHECK (venue_code BETWEEN 0 AND 255),
    venue text NOT NULL,
    first_event_ts_us bigint CHECK (first_event_ts_us >= 0),
    first_fill_ts_us bigint CHECK (first_fill_ts_us >= 0),
    last_fill_ts_us bigint CHECK (last_fill_ts_us >= 0),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (source_id, symbol, venue_code)
);

INSERT INTO cta_source_symbols (
    source_id, symbol, venue_code, venue,
    first_event_ts_us, first_fill_ts_us, last_fill_ts_us
)
SELECT
    source_id,
    symbol,
    venue_code,
    MAX(venue),
    MIN(event_ts_us),
    MIN(CASE WHEN update_ts_us > 0 THEN update_ts_us ELSE event_ts_us END)
        FILTER (WHERE amount_update > 0),
    MAX(CASE WHEN update_ts_us > 0 THEN update_ts_us ELSE event_ts_us END)
        FILTER (WHERE amount_update > 0)
FROM cta_uniform_order_events
WHERE symbol <> ''
GROUP BY source_id, symbol, venue_code;
