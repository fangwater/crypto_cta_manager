-- Explicit maintenance for an existing domestic Manager database; never run at startup.
BEGIN;
-- Manager-owned attribution only. No foreign key or write into execution stores.
CREATE TABLE cta_native_netting_portfolios (
    source_id text PRIMARY KEY REFERENCES cta_order_sources(source_id),
    state jsonb NOT NULL CHECK (jsonb_typeof(state) = 'object'),
    updated_at_us bigint NOT NULL CHECK (updated_at_us > 0)
);
CREATE TABLE cta_native_netting_members (
    source_id text NOT NULL REFERENCES cta_native_netting_portfolios(source_id),
    strategy_name text PRIMARY KEY REFERENCES cta_position_strategies(strategy_name)
);
CREATE TABLE cta_native_netting_events (
    source_id text NOT NULL REFERENCES cta_native_netting_portfolios(source_id),
    event_id text NOT NULL,
    revision bigint NOT NULL CHECK (revision >= 0),
    kind text NOT NULL CHECK (kind IN ('initialize','target_batch','exchange_fill_allocation','external_demand_release')),
    fingerprint text NOT NULL,
    batch_id text,
    factual_identity text,
    request jsonb NOT NULL,
    receipt jsonb NOT NULL,
    actor_user_id bigint NOT NULL REFERENCES cta_users(user_id),
    received_at_us bigint NOT NULL CHECK (received_at_us > 0),
    PRIMARY KEY(source_id,event_id),
    UNIQUE(source_id,revision)
);
CREATE UNIQUE INDEX cta_native_netting_factual_fill ON cta_native_netting_events(source_id,factual_identity) WHERE factual_identity IS NOT NULL;
CREATE TABLE cta_native_netting_demands (
    source_id text NOT NULL REFERENCES cta_native_netting_portfolios(source_id),
    batch_id text NOT NULL,
    strategy_name text NOT NULL REFERENCES cta_native_netting_members(strategy_name),
    market text NOT NULL,
    remaining_lots bigint NOT NULL,
    PRIMARY KEY(source_id,batch_id,strategy_name,market)
);
COMMIT;
