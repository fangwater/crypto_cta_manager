ALTER TABLE cta_position_strategies
    ADD COLUMN created_by_user_id bigint REFERENCES cta_users(user_id) ON DELETE SET NULL,
    -- SHA-256 hex of the per-strategy publish token. NULL or empty keeps the
    -- legacy open push behavior until a token is configured.
    ADD COLUMN publish_token_hash text;

CREATE TABLE cta_position_strategy_managers (
    strategy_name text NOT NULL REFERENCES cta_position_strategies(strategy_name) ON DELETE CASCADE,
    user_id bigint NOT NULL REFERENCES cta_users(user_id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (strategy_name, user_id)
);
