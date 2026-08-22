-- Keep the legacy compatibility column aligned with the unrestricted
-- maker/taker API. Only non-finite values are rejected.

ALTER TABLE cta_order_sources
    DROP CONSTRAINT IF EXISTS cta_order_sources_estimated_fee_rate_check;

ALTER TABLE cta_order_sources
    ADD CONSTRAINT cta_order_sources_estimated_fee_rate_check
    CHECK (
        estimated_fee_rate > '-Infinity'::double precision
        AND estimated_fee_rate < 'Infinity'::double precision
    );
