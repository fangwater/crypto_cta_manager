-- Explicit maintenance for append-only historical ownership reconstruction.
-- Existing positions, timestamps, notes and creation times remain immutable.
BEGIN;
SET LOCAL lock_timeout = '5s';
ALTER TABLE cta_strategy_allocation_snapshots
    ADD COLUMN restatements jsonb NOT NULL DEFAULT '[]'::jsonb
    CHECK (jsonb_typeof(restatements) = 'array');
COMMIT;
