-- Explicit maintenance only. Normal startup never runs DDL.
BEGIN;
SET LOCAL lock_timeout = '5s';
ALTER TABLE cta_order_sources ADD COLUMN IF NOT EXISTS quote_fee_rates jsonb DEFAULT '{}'::jsonb NOT NULL CHECK (jsonb_typeof(quote_fee_rates) = 'object');
COMMIT;
