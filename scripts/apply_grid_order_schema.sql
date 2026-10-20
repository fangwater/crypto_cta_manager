-- Explicit maintenance for an existing database, before deploying Grid support.
-- Review and run separately; application startup never executes this file.
BEGIN;
ALTER TABLE cta_order_strategies
    ADD COLUMN grid jsonb NOT NULL DEFAULT '{"open_buy_vol_scale":[0.0,1.0],"open_sell_vol_scale":[0.0,1.0],"open_offset_lower":0.0,"vol_stale_ms":15000,"quote_stale_ms":1000}'::jsonb,
    ADD CONSTRAINT cta_order_strategies_grid_check CHECK (jsonb_typeof(grid) = 'object');
ALTER TABLE cta_order_strategies DROP CONSTRAINT cta_order_strategies_algorithm_check;
ALTER TABLE cta_order_strategies ADD CONSTRAINT cta_order_strategies_algorithm_check
    CHECK (algorithm IN ('batch', 'pov', 'chase', 'ctaker', 'grid'));
COMMIT;
