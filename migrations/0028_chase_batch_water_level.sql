-- Chase contract replacement: size each target generation into dynamic batches
-- and express the open maker water level as batch-equivalents.

UPDATE cta_order_strategies
SET chase = (chase - 'single_order_usdt' - 'max_open_usdt')
    || jsonb_build_object(
        'batch_floor_usdt', COALESCE((chase->>'single_order_usdt')::numeric, 100.0),
        'max_batch', 4,
        'max_open_batches', 2,
        'strategy_order_rate_limit_per_min', 0,
        'strategy_order_rate_limit_10s', 0
    );

ALTER TABLE cta_order_strategies
    ALTER COLUMN chase SET DEFAULT '{
        "batch_floor_usdt": 100.0,
        "max_batch": 4,
        "max_open_batches": 2,
        "maker_recenter_trigger_bps": 5.0,
        "maker_amend_cooldown_ms": 1000,
        "maker_timeout_sec": 120,
        "target_tolerance_usdt": 10.0,
        "strategy_order_rate_limit_per_min": 0,
        "strategy_order_rate_limit_10s": 0
    }'::jsonb;
