-- Per-strategy visibility grants, separate from publish authorization in
-- cta_position_strategy_managers. No viewer rows means the strategy stays
-- visible to every logged-in user (legacy open behavior); once any viewer is
-- assigned, the strategy is visible only to admins, its creator, viewers, and
-- managers.
CREATE TABLE cta_position_strategy_viewers (
    strategy_name text NOT NULL REFERENCES cta_position_strategies(strategy_name) ON DELETE CASCADE,
    user_id bigint NOT NULL REFERENCES cta_users(user_id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (strategy_name, user_id)
);
