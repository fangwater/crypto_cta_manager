-- Explicit maintenance: external read-only account catalog and view grants.
-- No Exec sources, order copies or trading bindings are created.
BEGIN;
SET LOCAL lock_timeout = '5s';
CREATE TABLE cta_external_overview_accounts (
    source_id text PRIMARY KEY,
    provider_id text NOT NULL,
    upstream_id text NOT NULL,
    account_label text NOT NULL,
    venue_label text NOT NULL,
    enabled boolean NOT NULL DEFAULT true,
    UNIQUE (provider_id, upstream_id)
);
CREATE TABLE cta_external_overview_viewers (
    source_id text NOT NULL REFERENCES cta_external_overview_accounts(source_id),
    user_id bigint NOT NULL REFERENCES cta_users(user_id) ON DELETE CASCADE,
    PRIMARY KEY (source_id, user_id)
);
COMMIT;
