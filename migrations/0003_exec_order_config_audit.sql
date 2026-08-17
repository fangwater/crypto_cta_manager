CREATE TABLE cta_exec_order_config_audit (
    audit_id bigserial PRIMARY KEY,
    source_id text NOT NULL REFERENCES cta_order_sources(source_id),
    strategy_name text NOT NULL,
    client_addr text NOT NULL,
    expected_updated_at_us bigint,
    result_updated_at_us bigint,
    previous_order_parameters jsonb NOT NULL,
    requested_order_parameters jsonb NOT NULL,
    status text NOT NULL CHECK (status IN ('pending', 'applied', 'failed')),
    error text,
    attempted_at timestamptz NOT NULL DEFAULT now(),
    completed_at timestamptz
);

CREATE INDEX cta_exec_order_config_audit_source_time_idx
    ON cta_exec_order_config_audit (source_id, attempted_at DESC);
