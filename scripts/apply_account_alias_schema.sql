-- Explicit maintenance before deploying the account-alias feature.
-- Back up cta_order_sources before applying.
BEGIN;
ALTER TABLE cta_order_sources
    ADD COLUMN IF NOT EXISTS display_alias text
        CHECK (display_alias IS NULL OR (char_length(btrim(display_alias)) BETWEEN 1 AND 200));
COMMIT;
