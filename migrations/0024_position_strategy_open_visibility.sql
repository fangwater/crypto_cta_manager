-- Position strategies are private by default: only admins, the creator,
-- granted viewers, and publish managers can see them. open_visibility = true
-- restores the legacy all-users visibility. Preserve the effective state of
-- existing rows: a strategy with no viewer rows was open to every user.
ALTER TABLE cta_position_strategies
    ADD COLUMN open_visibility boolean;

UPDATE cta_position_strategies p
SET open_visibility = NOT EXISTS (
    SELECT 1 FROM cta_position_strategy_viewers v
    WHERE v.strategy_name = p.strategy_name
);

ALTER TABLE cta_position_strategies
    ALTER COLUMN open_visibility SET NOT NULL,
    ALTER COLUMN open_visibility SET DEFAULT false;
