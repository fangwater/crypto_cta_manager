-- Three-tier account access per (user, source): no row means the account is
-- hidden, 'view' is read-only, and 'configure' additionally allows every
-- account-scoped write (existing bindings, shares, execution parameters, fee
-- rates, and symbol contract leverage). Existing grants keep full configure
-- authority.
ALTER TABLE cta_user_source_permissions
    ADD COLUMN access_level text NOT NULL DEFAULT 'configure'
        CHECK (access_level IN ('view', 'configure'));

-- Per-strategy user grants, independent of account grants. 'configure'
-- implies 'view' and additionally authorizes binding the strategy to an
-- account and managing that strategy's view/configure grants. Publish
-- authority stays in cta_position_strategy_managers.
CREATE TABLE cta_position_strategy_grants (
    strategy_name text NOT NULL REFERENCES cta_position_strategies(strategy_name) ON DELETE CASCADE,
    user_id bigint NOT NULL REFERENCES cta_users(user_id) ON DELETE CASCADE,
    access_level text NOT NULL CHECK (access_level IN ('view', 'configure')),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (strategy_name, user_id)
);

INSERT INTO cta_position_strategy_grants (strategy_name, user_id, access_level)
    SELECT strategy_name, user_id, 'view' FROM cta_position_strategy_viewers;

DROP TABLE cta_position_strategy_viewers;
