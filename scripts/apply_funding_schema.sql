-- Explicit additive maintenance for an existing Manager database.
-- No trading tables or Exec stores are changed. Run with psql ON_ERROR_STOP.
BEGIN;
-- Opt-in, exchange-owned funding ledger. Exec orders remain in Exec RocksDB.
CREATE TABLE cta_funding_accounts (
    source_id text PRIMARY KEY REFERENCES cta_order_sources(source_id),
    start_ts_us bigint NOT NULL CHECK (start_ts_us > 0)
);
CREATE TABLE cta_funding_symbols (
    source_id text NOT NULL REFERENCES cta_funding_accounts(source_id),
    venue text NOT NULL,
    symbol text NOT NULL,
    start_ts_us bigint NOT NULL CHECK (start_ts_us > 0),
    synced_through_ts_us bigint,
    PRIMARY KEY (source_id, venue, symbol)
);
CREATE TABLE cta_funding_income (
    source_id text NOT NULL,
    venue text NOT NULL,
    tran_id text NOT NULL,
    symbol text NOT NULL,
    asset text NOT NULL CHECK (asset IN ('USDT', 'USDC')),
    ts_us bigint NOT NULL,
    income numeric NOT NULL,
    PRIMARY KEY (source_id, venue, tran_id),
    FOREIGN KEY (source_id, venue, symbol) REFERENCES cta_funding_symbols(source_id, venue, symbol)
);
CREATE INDEX cta_funding_income_source_time_idx ON cta_funding_income(source_id, ts_us);
COMMIT;
