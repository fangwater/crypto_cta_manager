-- Explicit maintenance for the separate CN Manager database. No startup DDL.
BEGIN;
CREATE TABLE cn_futures_nav_snapshots (
    source_id text NOT NULL REFERENCES cta_order_sources(source_id),
    snapshot_us bigint NOT NULL CHECK (snapshot_us > 0),
    anchor jsonb NOT NULL CHECK (jsonb_typeof(anchor) = 'object'),
    created_by text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (source_id, snapshot_us),
    CHECK (anchor->>'source_id' = source_id),
    CHECK ((anchor->>'snapshot_us')::bigint = snapshot_us)
);
COMMIT;
