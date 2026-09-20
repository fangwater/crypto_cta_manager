ALTER TABLE cta_order_strategies
    ADD COLUMN algorithm text NOT NULL DEFAULT 'batch'
        CHECK (algorithm IN ('batch', 'pov', 'chase')),
    ADD COLUMN pov jsonb NOT NULL DEFAULT '{
        "participation_rate": 0.1,
        "max_batch_usdt": 300.0,
        "max_carry_usdt": 600.0,
        "volume_stale_ms": 5000,
        "quote_stale_ms": 1000,
        "duration_ms": 3600000,
        "liquidity": "maker_then_taker",
        "limit_price": null
    }'::jsonb CHECK (jsonb_typeof(pov) = 'object'),
    ADD COLUMN chase jsonb NOT NULL DEFAULT '{
        "single_order_usdt": 100.0,
        "max_open_usdt": 200.0,
        "maker_recenter_trigger_bps": 3.0,
        "maker_amend_cooldown_ms": 0,
        "maker_timeout_ms": 60000,
        "target_tolerance_usdt": 10.0,
        "bbo_max_age_ms": 2000
    }'::jsonb CHECK (jsonb_typeof(chase) = 'object');
