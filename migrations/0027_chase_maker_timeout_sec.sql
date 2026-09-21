-- Chase contract replacement: maker_timeout_ms -> maker_timeout_sec and the
-- bbo_max_age_ms field is removed. Rewrite stored chase jsonb templates so the
-- strict Exec/Manager parsers keep accepting existing rows.

UPDATE cta_order_strategies
SET chase = CASE
        WHEN chase ? 'maker_timeout_ms' THEN
            (chase - 'bbo_max_age_ms' - 'maker_timeout_ms')
            || jsonb_build_object(
                'maker_timeout_sec',
                GREATEST(1, CEIL((chase->>'maker_timeout_ms')::numeric / 1000.0))::int
            )
        ELSE chase - 'bbo_max_age_ms'
    END
WHERE chase ? 'maker_timeout_ms' OR chase ? 'bbo_max_age_ms';

ALTER TABLE cta_order_strategies
    ALTER COLUMN chase SET DEFAULT '{
        "single_order_usdt": 100.0,
        "max_open_usdt": 200.0,
        "maker_recenter_trigger_bps": 5.0,
        "maker_amend_cooldown_ms": 0,
        "maker_timeout_sec": 120,
        "target_tolerance_usdt": 10.0
    }'::jsonb;
