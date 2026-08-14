CREATE TABLE cta_order_sources (
    source_id text PRIMARY KEY,
    account_label text NOT NULL,
    venue_label text NOT NULL,
    rocksdb_path text NOT NULL,
    enabled boolean NOT NULL DEFAULT true,
    last_success_at timestamptz,
    last_error_at timestamptz,
    last_error text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE cta_ingestion_checkpoints (
    source_id text NOT NULL REFERENCES cta_order_sources(source_id),
    stream_name text NOT NULL,
    next_ts_us bigint NOT NULL CHECK (next_ts_us >= 0),
    last_scan_start_ts_us bigint NOT NULL CHECK (last_scan_start_ts_us >= 0),
    last_event_count bigint NOT NULL DEFAULT 0 CHECK (last_event_count >= 0),
    last_decode_failure_count bigint NOT NULL DEFAULT 0 CHECK (last_decode_failure_count >= 0),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (source_id, stream_name)
);

CREATE TABLE cta_uniform_order_events (
    source_id text NOT NULL REFERENCES cta_order_sources(source_id),
    record_key text NOT NULL,
    event_ts_us bigint NOT NULL CHECK (event_ts_us >= 0),
    recv_ts_us bigint NOT NULL,
    symbol text NOT NULL,
    create_ts_us bigint NOT NULL,
    update_ts_us bigint NOT NULL,
    signal_ts_us bigint NOT NULL,
    submit_ts_us bigint NOT NULL,
    local_ts_us bigint NOT NULL,
    market_ts_us bigint NOT NULL,
    client_order_id bigint NOT NULL,
    venue_code smallint NOT NULL CHECK (venue_code BETWEEN 0 AND 255),
    venue text NOT NULL,
    order_type_code smallint NOT NULL CHECK (order_type_code BETWEEN 0 AND 255),
    order_type text NOT NULL,
    side_code smallint NOT NULL CHECK (side_code BETWEEN 0 AND 255),
    side text NOT NULL,
    price double precision NOT NULL,
    price_offset double precision NOT NULL,
    amount_initial double precision NOT NULL,
    amount_update double precision NOT NULL,
    status_code smallint NOT NULL CHECK (status_code BETWEEN 0 AND 255),
    status text NOT NULL,
    from_key bytea NOT NULL,
    from_key_text text NOT NULL,
    bbo_spread text NOT NULL,
    signal_open_venue_code smallint CHECK (signal_open_venue_code BETWEEN 0 AND 255),
    signal_open_ts_us bigint,
    signal_open_bid_price double precision,
    signal_open_bid_quantity double precision,
    signal_open_ask_price double precision,
    signal_open_ask_quantity double precision,
    signal_hedge_venue_code smallint CHECK (signal_hedge_venue_code BETWEEN 0 AND 255),
    signal_hedge_ts_us bigint,
    signal_hedge_bid_price double precision,
    signal_hedge_bid_quantity double precision,
    signal_hedge_ask_price double precision,
    signal_hedge_ask_quantity double precision,
    wire_version smallint NOT NULL,
    wire_payload bytea NOT NULL,
    ingested_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (source_id, record_key)
);

CREATE INDEX cta_uniform_order_events_source_event_idx
    ON cta_uniform_order_events (source_id, event_ts_us);

CREATE INDEX cta_uniform_order_events_source_client_idx
    ON cta_uniform_order_events (source_id, client_order_id, event_ts_us);

CREATE INDEX cta_uniform_order_events_source_symbol_idx
    ON cta_uniform_order_events (source_id, symbol, event_ts_us);

CREATE TABLE cta_ingestion_failures (
    source_id text NOT NULL REFERENCES cta_order_sources(source_id),
    stream_name text NOT NULL,
    record_key bytea NOT NULL,
    wire_payload bytea NOT NULL,
    error text NOT NULL,
    first_seen_at timestamptz NOT NULL DEFAULT now(),
    last_seen_at timestamptz NOT NULL DEFAULT now(),
    occurrence_count bigint NOT NULL DEFAULT 1 CHECK (occurrence_count > 0),
    PRIMARY KEY (source_id, stream_name, record_key)
);
