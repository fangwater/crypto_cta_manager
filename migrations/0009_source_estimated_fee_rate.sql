-- Per-source estimated trading fee rate for NAV reconstruction.
-- Stored in PostgreSQL so operators can change it without restarting cta_web.
-- Value is a fraction (e.g. 0.0004 = 4 bps), not basis points.

ALTER TABLE cta_order_sources
    ADD COLUMN IF NOT EXISTS estimated_fee_rate double precision;

UPDATE cta_order_sources
SET estimated_fee_rate = 0.0004
WHERE estimated_fee_rate IS NULL;

ALTER TABLE cta_order_sources
    ALTER COLUMN estimated_fee_rate SET DEFAULT 0.0004;

ALTER TABLE cta_order_sources
    ALTER COLUMN estimated_fee_rate SET NOT NULL;

ALTER TABLE cta_order_sources
    DROP CONSTRAINT IF EXISTS cta_order_sources_estimated_fee_rate_check;

ALTER TABLE cta_order_sources
    ADD CONSTRAINT cta_order_sources_estimated_fee_rate_check
    CHECK (estimated_fee_rate >= 0 AND estimated_fee_rate = estimated_fee_rate);
