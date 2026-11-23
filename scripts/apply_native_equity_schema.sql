-- Explicit maintenance for the separate CN Manager database. No startup DDL.
-- One row per source and native trading day (night sessions belong to the
-- following trading day): the latest broker funds observation of that day and
-- the gross position notionals Manager replayed at about the same time.
BEGIN;
CREATE TABLE cn_futures_account_equity_daily (
    source_id text NOT NULL REFERENCES cta_order_sources(source_id),
    trading_day text NOT NULL CHECK (trading_day ~ '^[0-9]{8}$'),
    observed_us bigint NOT NULL CHECK (observed_us > 0),
    funds jsonb NOT NULL CHECK (jsonb_typeof(funds) = 'object'),
    long_notional double precision,
    short_notional double precision,
    positions_at_us bigint CHECK (positions_at_us > 0),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (source_id, trading_day),
    CHECK (funds->>'trading_day' = trading_day),
    CHECK ((funds->>'observed_us')::bigint = observed_us)
);
COMMIT;
