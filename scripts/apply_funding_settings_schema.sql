-- Explicit maintenance for existing Manager databases. No startup DDL.
BEGIN;
ALTER TABLE cta_order_sources
    ADD COLUMN funding_enabled boolean NOT NULL DEFAULT false,
    ADD COLUMN funding_refresh_secs integer NOT NULL DEFAULT 300 CHECK (funding_refresh_secs BETWEEN 60 AND 86400),
    ADD COLUMN funding_verified_at_us bigint,
    ADD COLUMN funding_verified_local_ip text,
    ADD COLUMN funding_verified_public_ip text;
COMMIT;
