CREATE TABLE cta_strategy_position_snapshots (
    source_id text NOT NULL REFERENCES cta_order_sources(source_id),
    snapshot_ts_us bigint NOT NULL CHECK (snapshot_ts_us > 0),
    note text,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (source_id, snapshot_ts_us)
);

CREATE TABLE cta_strategy_position_snapshot_entries (
    source_id text NOT NULL,
    snapshot_ts_us bigint NOT NULL,
    strategy_name text NOT NULL CHECK (length(strategy_name) > 0),
    symbol text NOT NULL CHECK (length(symbol) > 0),
    venue_code smallint NOT NULL CHECK (venue_code BETWEEN 0 AND 255),
    quantity double precision NOT NULL CHECK (
        quantity <> 0
        AND quantity <> 'NaN'::double precision
        AND abs(quantity) <> 'Infinity'::double precision
    ),
    reference_price double precision NOT NULL CHECK (
        reference_price > 0
        AND reference_price <> 'NaN'::double precision
        AND reference_price <> 'Infinity'::double precision
    ),
    PRIMARY KEY (source_id, snapshot_ts_us, strategy_name, symbol, venue_code),
    FOREIGN KEY (source_id, snapshot_ts_us)
        REFERENCES cta_strategy_position_snapshots(source_id, snapshot_ts_us)
        ON DELETE CASCADE
);

CREATE INDEX cta_strategy_position_snapshots_source_latest_idx
    ON cta_strategy_position_snapshots (source_id, snapshot_ts_us DESC);
