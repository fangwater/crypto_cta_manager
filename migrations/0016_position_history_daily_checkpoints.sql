-- Derived cache for the position-history endpoint.  It is deliberately
-- separate from cta_position_snapshots, whose rows are immutable anchors.
CREATE TABLE cta_position_history_sources (
    source_id text PRIMARY KEY REFERENCES cta_order_sources(source_id),
    anchor_fingerprint text NOT NULL,
    effective_anchor_ts_us bigint CHECK (effective_anchor_ts_us >= 0),
    scanned_recv_ts_us bigint NOT NULL CHECK (scanned_recv_ts_us >= 0),
    recent_records jsonb NOT NULL DEFAULT '[]'::jsonb,
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- One complete portfolio state strictly before each UTC day_start_us. JSON
-- keeps an explicit empty portfolio, distinct from a missing checkpoint.
-- The first partial day is seeded at effective_anchor_ts_us and is unavailable
-- before that source anchor, even though its lookup key is the UTC day start.
CREATE TABLE cta_position_history_daily_checkpoints (
    source_id text NOT NULL REFERENCES cta_order_sources(source_id),
    day_start_us bigint NOT NULL CHECK (day_start_us >= 0),
    anchor_fingerprint text NOT NULL,
    -- Exclusive receive-key upper bound required to replay fills for this
    -- event-time day, including fills received after the day closed.
    fills_recv_end_us bigint NOT NULL DEFAULT 0 CHECK (fills_recv_end_us >= 0),
    positions jsonb NOT NULL,
    completed_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (source_id, day_start_us)
);
