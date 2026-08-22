-- Per-source maker/taker fee schedule for NAV reconstruction.
-- Rates are fractions: -0.00005 = 0.50 bps rebate, 0.000146 = 1.46 bps cost.

ALTER TABLE cta_order_sources
    ADD COLUMN IF NOT EXISTS maker_fee_rate double precision;

ALTER TABLE cta_order_sources
    ADD COLUMN IF NOT EXISTS taker_fee_rate double precision;

UPDATE cta_order_sources
SET maker_fee_rate = estimated_fee_rate
WHERE maker_fee_rate IS NULL;

UPDATE cta_order_sources
SET taker_fee_rate = estimated_fee_rate
WHERE taker_fee_rate IS NULL;

ALTER TABLE cta_order_sources
    ALTER COLUMN maker_fee_rate SET DEFAULT 0.0004,
    ALTER COLUMN maker_fee_rate SET NOT NULL,
    ALTER COLUMN taker_fee_rate SET DEFAULT 0.0004,
    ALTER COLUMN taker_fee_rate SET NOT NULL;

ALTER TABLE cta_order_sources
    ADD CONSTRAINT cta_order_sources_maker_fee_rate_check
    CHECK (
        maker_fee_rate > '-Infinity'::double precision
        AND maker_fee_rate < 'Infinity'::double precision
    );

ALTER TABLE cta_order_sources
    ADD CONSTRAINT cta_order_sources_taker_fee_rate_check
    CHECK (
        taker_fee_rate > '-Infinity'::double precision
        AND taker_fee_rate < 'Infinity'::double precision
    );
