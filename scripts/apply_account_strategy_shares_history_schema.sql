-- Explicit maintenance for an existing Manager database, after a backup.
-- Install before the matching Manager binary/frontend. Existing account
-- allocations and targets are preserved; history starts on the next change.
BEGIN;
SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '30s';

CREATE TABLE cta_account_strategy_shares_history (
    change_id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    source_id text NOT NULL,
    strategy_name text NOT NULL,
    initial_shares double precision NOT NULL CHECK (initial_shares >= 0 AND initial_shares < 'Infinity'::double precision),
    final_shares double precision NOT NULL CHECK (final_shares >= 0 AND final_shares < 'Infinity'::double precision),
    first_changed_at_us bigint NOT NULL,
    last_changed_at_us bigint NOT NULL CHECK (last_changed_at_us >= first_changed_at_us),
    change_count bigint NOT NULL DEFAULT 1 CHECK (change_count > 0),
    actors jsonb NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(actors) = 'array'),
    origins text[] NOT NULL
);
-- Historical account and leaf-strategy identity survives catalog deletion.
CREATE INDEX cta_account_strategy_shares_history_strategy_idx
    ON cta_account_strategy_shares_history (source_id, strategy_name, change_id DESC);
CREATE INDEX cta_account_strategy_shares_history_time_idx
    ON cta_account_strategy_shares_history (source_id, last_changed_at_us DESC, change_id DESC)
    WHERE initial_shares IS DISTINCT FROM final_shares;

COMMIT;
