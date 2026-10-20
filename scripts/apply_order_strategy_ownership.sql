-- Explicit maintenance before deploying order-template ownership support.
-- Existing templates retain shared edit access; new templates belong to their creator.
BEGIN;
ALTER TABLE cta_order_strategies
    ADD COLUMN created_by_user_id bigint REFERENCES cta_users(user_id);
COMMIT;
