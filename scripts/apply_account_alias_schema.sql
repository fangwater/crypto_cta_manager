-- Explicit maintenance before deploying the account-alias feature.
-- Back up existing account aliases before applying.
BEGIN;
ALTER TABLE cta_order_sources
    ADD COLUMN IF NOT EXISTS display_alias text
        CHECK (display_alias IS NULL OR (char_length(btrim(display_alias)) BETWEEN 1 AND 200));
CREATE TABLE IF NOT EXISTS cta_managed_account_aliases (
    account_id text PRIMARY KEY,
    display_alias text NOT NULL CHECK (char_length(btrim(display_alias)) BETWEEN 1 AND 200),
    updated_at timestamp with time zone DEFAULT now() NOT NULL
);
COMMIT;
