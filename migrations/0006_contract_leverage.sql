CREATE TABLE cta_account_symbol_leverages (
    source_id text NOT NULL REFERENCES cta_order_sources(source_id),
    symbol text NOT NULL,
    contract_leverage integer NOT NULL CHECK (contract_leverage >= 1 AND contract_leverage <= 125),
    updated_at_us bigint NOT NULL,
    PRIMARY KEY (source_id, symbol)
);
