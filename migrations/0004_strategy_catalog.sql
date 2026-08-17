CREATE TABLE cta_position_strategies (
    strategy_name text PRIMARY KEY,
    equity_usdt double precision NOT NULL CHECK (equity_usdt > 0),
    targets jsonb NOT NULL DEFAULT '{}'::jsonb,
    updated_at_us bigint NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE cta_order_strategies (
    strategy_name text PRIMARY KEY,
    single_order_usdt double precision NOT NULL CHECK (single_order_usdt > 0),
    orders_per_batch integer NOT NULL CHECK (orders_per_batch > 0),
    maker_price_anchor text NOT NULL,
    tick_spacing integer NOT NULL CHECK (tick_spacing >= 0),
    batch_interval_ms integer NOT NULL CHECK (batch_interval_ms >= 0),
    maker_timeout_ms integer NOT NULL CHECK (maker_timeout_ms > 0),
    max_maker_requotes integer NOT NULL CHECK (max_maker_requotes >= 0),
    target_tolerance_usdt double precision NOT NULL CHECK (target_tolerance_usdt >= 0),
    updated_at_us bigint NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE cta_account_settings (
    source_id text PRIMARY KEY REFERENCES cta_order_sources(source_id),
    leverage double precision NOT NULL CHECK (leverage > 0),
    updated_at_us bigint NOT NULL
);

CREATE TABLE cta_account_strategy_bindings (
    source_id text NOT NULL REFERENCES cta_order_sources(source_id),
    binding_name text NOT NULL,
    position_strategy_name text NOT NULL REFERENCES cta_position_strategies(strategy_name),
    order_strategy_name text NOT NULL REFERENCES cta_order_strategies(strategy_name),
    updated_at_us bigint NOT NULL,
    PRIMARY KEY (source_id, binding_name)
);

CREATE INDEX cta_account_strategy_bindings_position_idx
    ON cta_account_strategy_bindings (position_strategy_name);

CREATE INDEX cta_account_strategy_bindings_order_idx
    ON cta_account_strategy_bindings (order_strategy_name);
