ALTER TABLE cta_position_strategies
    ADD COLUMN symbol_order_strategy_overrides jsonb NOT NULL DEFAULT '{}'::jsonb
    CHECK (jsonb_typeof(symbol_order_strategy_overrides) = 'object');
