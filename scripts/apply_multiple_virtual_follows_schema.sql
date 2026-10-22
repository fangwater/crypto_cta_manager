-- Explicit maintenance for an existing Manager database. Back up first and
-- deploy the matching Manager binary/frontend together; never run at startup.
-- Existing follow rows, effective bindings and publication queues are preserved.
BEGIN;
SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '30s';
SELECT pg_advisory_xact_lock(740913211);
ALTER TABLE cta_account_follows DROP CONSTRAINT cta_account_follows_pkey;
ALTER TABLE cta_account_follows ADD PRIMARY KEY (source_id, virtual_id);
COMMIT;
