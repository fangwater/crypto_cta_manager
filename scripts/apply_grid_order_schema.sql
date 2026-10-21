-- Explicit maintenance for an existing database, before deploying Grid support.
-- Review and run separately; application startup never executes this file.
BEGIN;
ALTER TABLE cta_order_strategies
    ADD COLUMN grid jsonb NOT NULL DEFAULT '{"spot":{"orders_per_batch":3,"maker_timeout_ms":1000,"vol_multiplier":1.0,"max_maker_requotes":2},"futures":{"orders_per_batch":3,"maker_timeout_ms":1000,"vol_multiplier":1.0,"max_maker_requotes":null}}'::jsonb,
    ADD CONSTRAINT cta_order_strategies_grid_check CHECK (jsonb_typeof(grid) = 'object');
ALTER TABLE cta_order_strategies DROP CONSTRAINT cta_order_strategies_algorithm_check;
ALTER TABLE cta_order_strategies ADD CONSTRAINT cta_order_strategies_algorithm_check
    CHECK (algorithm IN ('batch', 'pov', 'chase', 'ctaker', 'grid'));
COMMIT;
