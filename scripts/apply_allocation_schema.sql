-- Explicit maintenance only; normal startup never runs schema DDL.
BEGIN;
CREATE TABLE cta_strategy_allocation_snapshots (
    source_id text NOT NULL REFERENCES cta_order_sources(source_id),
    snapshot_ts_us bigint NOT NULL CHECK (snapshot_ts_us > 0),
    positions jsonb NOT NULL CHECK (jsonb_typeof(positions) = 'array' AND jsonb_array_length(positions) > 0),
    note text NOT NULL CHECK (length(note) > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (source_id, snapshot_ts_us)
);
COMMIT;
