-- Incrementally materialized portfolio NAV for theoretical 5-minute TWAP
-- executions. Five-minute mark points are stored only while a position is open.

ALTER TABLE cta_order_sources
    ADD COLUMN theoretical_twap_fee_rate double precision;

UPDATE cta_order_sources
SET theoretical_twap_fee_rate = maker_fee_rate * 0.5 + taker_fee_rate * 0.5
WHERE theoretical_twap_fee_rate IS NULL;

ALTER TABLE cta_order_sources
    ALTER COLUMN theoretical_twap_fee_rate SET DEFAULT 0.0004,
    ALTER COLUMN theoretical_twap_fee_rate SET NOT NULL;

ALTER TABLE cta_order_sources
    ADD CONSTRAINT cta_order_sources_theoretical_twap_fee_rate_check
    CHECK (
        theoretical_twap_fee_rate > '-Infinity'::double precision
        AND theoretical_twap_fee_rate < 'Infinity'::double precision
    );

CREATE TABLE cta_theoretical_nav_checkpoint (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    last_received_at_us bigint NOT NULL CHECK (last_received_at_us >= 0),
    last_seq bigint NOT NULL CHECK (last_seq BETWEEN 0 AND 4294967295),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE cta_theoretical_nav_pending (
    source_id text NOT NULL REFERENCES cta_order_sources(source_id),
    binding_name text NOT NULL CHECK (length(binding_name) > 0),
    position_strategy_name text NOT NULL CHECK (length(position_strategy_name) > 0),
    received_at_us bigint NOT NULL CHECK (received_at_us > 0),
    update_seq bigint NOT NULL CHECK (update_seq BETWEEN 0 AND 4294967295),
    window_end_us bigint NOT NULL CHECK (window_end_us >= received_at_us),
    venue text NOT NULL CHECK (length(venue) > 0),
    fee_rate double precision NOT NULL CHECK (
        fee_rate > '-Infinity'::double precision
        AND fee_rate < 'Infinity'::double precision
    ),
    targets jsonb NOT NULL CHECK (jsonb_typeof(targets) = 'object'),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (source_id, binding_name, received_at_us, update_seq)
);

CREATE INDEX cta_theoretical_nav_pending_due_idx
    ON cta_theoretical_nav_pending (window_end_us, received_at_us, update_seq);

CREATE TABLE cta_theoretical_binding_positions (
    source_id text NOT NULL REFERENCES cta_order_sources(source_id),
    binding_name text NOT NULL CHECK (length(binding_name) > 0),
    symbol text NOT NULL CHECK (length(symbol) > 0),
    venue text NOT NULL CHECK (length(venue) > 0),
    position_strategy_name text NOT NULL CHECK (length(position_strategy_name) > 0),
    quantity double precision NOT NULL CHECK (
        quantity <> 'NaN'::double precision
        AND abs(quantity) <> 'Infinity'::double precision
    ),
    updated_at_us bigint NOT NULL CHECK (updated_at_us > 0),
    update_seq bigint NOT NULL CHECK (update_seq BETWEEN 0 AND 4294967295),
    PRIMARY KEY (source_id, binding_name, symbol, venue)
);

CREATE TABLE cta_theoretical_nav_symbol_states (
    source_id text NOT NULL REFERENCES cta_order_sources(source_id),
    symbol text NOT NULL CHECK (length(symbol) > 0),
    venue text NOT NULL CHECK (length(venue) > 0),
    net_quantity double precision NOT NULL DEFAULT 0 CHECK (
        net_quantity <> 'NaN'::double precision
        AND abs(net_quantity) <> 'Infinity'::double precision
    ),
    realized_pnl_before_fee_quote double precision NOT NULL DEFAULT 0 CHECK (
        realized_pnl_before_fee_quote <> 'NaN'::double precision
        AND abs(realized_pnl_before_fee_quote) <> 'Infinity'::double precision
    ),
    estimated_trading_fee_quote double precision NOT NULL DEFAULT 0 CHECK (
        estimated_trading_fee_quote <> 'NaN'::double precision
        AND abs(estimated_trading_fee_quote) <> 'Infinity'::double precision
    ),
    mark_price double precision CHECK (
        mark_price > 0
        AND mark_price <> 'NaN'::double precision
        AND mark_price <> 'Infinity'::double precision
    ),
    next_lot_seq bigint NOT NULL DEFAULT 1 CHECK (next_lot_seq > 0),
    updated_at_us bigint NOT NULL CHECK (updated_at_us > 0),
    PRIMARY KEY (source_id, symbol, venue)
);

CREATE TABLE cta_theoretical_nav_fifo_lots (
    source_id text NOT NULL,
    symbol text NOT NULL,
    venue text NOT NULL,
    lot_seq bigint NOT NULL CHECK (lot_seq > 0),
    quantity double precision NOT NULL CHECK (
        quantity <> 0
        AND quantity <> 'NaN'::double precision
        AND abs(quantity) <> 'Infinity'::double precision
    ),
    entry_price double precision NOT NULL CHECK (
        entry_price > 0
        AND entry_price <> 'NaN'::double precision
        AND entry_price <> 'Infinity'::double precision
    ),
    PRIMARY KEY (source_id, symbol, venue, lot_seq),
    FOREIGN KEY (source_id, symbol, venue)
        REFERENCES cta_theoretical_nav_symbol_states(source_id, symbol, venue)
        ON DELETE CASCADE
);

CREATE TABLE cta_theoretical_nav_events (
    event_id bigserial PRIMARY KEY,
    source_id text NOT NULL REFERENCES cta_order_sources(source_id),
    binding_name text NOT NULL CHECK (length(binding_name) > 0),
    position_strategy_name text NOT NULL CHECK (length(position_strategy_name) > 0),
    symbol text NOT NULL CHECK (length(symbol) > 0),
    venue text NOT NULL CHECK (length(venue) > 0),
    received_at_us bigint NOT NULL CHECK (received_at_us > 0),
    update_seq bigint NOT NULL CHECK (update_seq BETWEEN 0 AND 4294967295),
    execution_ts_us bigint NOT NULL CHECK (execution_ts_us >= received_at_us),
    previous_quantity double precision NOT NULL CHECK (
        previous_quantity > '-Infinity'::double precision
        AND previous_quantity < 'Infinity'::double precision
    ),
    target_quantity double precision NOT NULL CHECK (
        target_quantity > '-Infinity'::double precision
        AND target_quantity < 'Infinity'::double precision
    ),
    executed_quantity double precision NOT NULL CHECK (
        executed_quantity <> 0
        AND executed_quantity > '-Infinity'::double precision
        AND executed_quantity < 'Infinity'::double precision
    ),
    twap_price double precision NOT NULL CHECK (
        twap_price > 0
        AND twap_price < 'Infinity'::double precision
    ),
    fee_rate double precision NOT NULL CHECK (
        fee_rate > '-Infinity'::double precision
        AND fee_rate < 'Infinity'::double precision
    ),
    fee_quote double precision NOT NULL CHECK (
        fee_quote > '-Infinity'::double precision
        AND fee_quote < 'Infinity'::double precision
    ),
    cumulative_realized_pnl_before_fee_quote double precision NOT NULL CHECK (
        cumulative_realized_pnl_before_fee_quote > '-Infinity'::double precision
        AND cumulative_realized_pnl_before_fee_quote < 'Infinity'::double precision
    ),
    cumulative_estimated_trading_fee_quote double precision NOT NULL CHECK (
        cumulative_estimated_trading_fee_quote > '-Infinity'::double precision
        AND cumulative_estimated_trading_fee_quote < 'Infinity'::double precision
    ),
    cumulative_floating_pnl_quote double precision NOT NULL CHECK (
        cumulative_floating_pnl_quote > '-Infinity'::double precision
        AND cumulative_floating_pnl_quote < 'Infinity'::double precision
    ),
    cumulative_nav_before_fee_quote double precision NOT NULL CHECK (
        cumulative_nav_before_fee_quote > '-Infinity'::double precision
        AND cumulative_nav_before_fee_quote < 'Infinity'::double precision
    ),
    cumulative_nav_after_fee_quote double precision NOT NULL CHECK (
        cumulative_nav_after_fee_quote > '-Infinity'::double precision
        AND cumulative_nav_after_fee_quote < 'Infinity'::double precision
    ),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (source_id, binding_name, symbol, venue, received_at_us, update_seq)
);

CREATE INDEX cta_theoretical_nav_events_timeline_idx
    ON cta_theoretical_nav_events (
        source_id, symbol, venue, execution_ts_us, received_at_us, update_seq
    );

CREATE INDEX cta_theoretical_nav_events_time_idx
    ON cta_theoretical_nav_events (
        execution_ts_us, received_at_us, update_seq, event_id
    );

CREATE TABLE cta_theoretical_nav_skips (
    source_id text NOT NULL REFERENCES cta_order_sources(source_id),
    binding_name text NOT NULL CHECK (length(binding_name) > 0),
    position_strategy_name text NOT NULL CHECK (length(position_strategy_name) > 0),
    symbol text NOT NULL CHECK (length(symbol) > 0),
    venue text NOT NULL CHECK (length(venue) > 0),
    received_at_us bigint NOT NULL CHECK (received_at_us > 0),
    update_seq bigint NOT NULL CHECK (update_seq BETWEEN 0 AND 4294967295),
    window_end_us bigint NOT NULL,
    reason text NOT NULL CHECK (length(reason) > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (source_id, binding_name, symbol, venue, received_at_us, update_seq)
);

CREATE TABLE cta_theoretical_nav_mark_checkpoints (
    source_id text PRIMARY KEY REFERENCES cta_order_sources(source_id),
    last_mark_ts_us bigint NOT NULL CHECK (last_mark_ts_us >= 0),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE cta_theoretical_nav_portfolio_points (
    source_id text NOT NULL REFERENCES cta_order_sources(source_id),
    ts_us bigint NOT NULL CHECK (ts_us > 0),
    point_kind text NOT NULL CHECK (point_kind IN ('execution', 'mark')),
    open_position_count integer NOT NULL CHECK (open_position_count >= 0),
    cumulative_realized_pnl_before_fee_quote double precision NOT NULL CHECK (
        cumulative_realized_pnl_before_fee_quote > '-Infinity'::double precision
        AND cumulative_realized_pnl_before_fee_quote < 'Infinity'::double precision
    ),
    cumulative_estimated_trading_fee_quote double precision NOT NULL CHECK (
        cumulative_estimated_trading_fee_quote > '-Infinity'::double precision
        AND cumulative_estimated_trading_fee_quote < 'Infinity'::double precision
    ),
    cumulative_floating_pnl_quote double precision NOT NULL CHECK (
        cumulative_floating_pnl_quote > '-Infinity'::double precision
        AND cumulative_floating_pnl_quote < 'Infinity'::double precision
    ),
    cumulative_nav_before_fee_quote double precision NOT NULL CHECK (
        cumulative_nav_before_fee_quote > '-Infinity'::double precision
        AND cumulative_nav_before_fee_quote < 'Infinity'::double precision
    ),
    cumulative_nav_after_fee_quote double precision NOT NULL CHECK (
        cumulative_nav_after_fee_quote > '-Infinity'::double precision
        AND cumulative_nav_after_fee_quote < 'Infinity'::double precision
    ),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (source_id, ts_us)
);

CREATE INDEX cta_theoretical_nav_portfolio_points_time_idx
    ON cta_theoretical_nav_portfolio_points (ts_us, source_id);
