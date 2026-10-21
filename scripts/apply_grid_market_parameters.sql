-- Explicit maintenance: normalize Grid to the current per-market execution settings.
-- Preserve each template's existing split count, maker duration and spot round limit.
BEGIN;
SET LOCAL lock_timeout = '5s';
ALTER TABLE cta_order_strategies ALTER COLUMN grid SET DEFAULT '{"spot":{"orders_per_batch":3,"maker_timeout_ms":1000,"vol_multiplier":1.0,"max_maker_requotes":2},"futures":{"orders_per_batch":3,"maker_timeout_ms":1000,"vol_multiplier":1.0,"max_maker_requotes":null}}'::jsonb;
UPDATE cta_order_strategies
SET grid = jsonb_build_object(
    'spot', jsonb_build_object('orders_per_batch', orders_per_batch,
        'maker_timeout_ms', maker_timeout_ms, 'max_maker_requotes', max_maker_requotes),
    'futures', jsonb_build_object('orders_per_batch', orders_per_batch,
        'maker_timeout_ms', maker_timeout_ms))
WHERE NOT (grid ? 'spot' AND grid ? 'futures');
-- Add missing controls without changing existing multipliers, limits (including null),
-- split counts, timeouts, catalog timestamps or non-Grid order settings.
UPDATE cta_order_strategies
SET grid = jsonb_build_object(
    'spot', '{"vol_multiplier":1.0,"max_maker_requotes":2}'::jsonb || (grid->'spot'),
    'futures', '{"vol_multiplier":1.0,"max_maker_requotes":null}'::jsonb || (grid->'futures'))
WHERE NOT ((grid->'spot') ?& ARRAY['vol_multiplier', 'max_maker_requotes']
       AND (grid->'futures') ?& ARRAY['vol_multiplier', 'max_maker_requotes']);
COMMIT;
