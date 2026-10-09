-- Current Manager database schema. Actual orders and fills remain in Exec RocksDB.
-- Run explicitly on a new, empty database, within one transaction.

CREATE SEQUENCE cta_auth_sessions_session_id_seq AS bigint
    START WITH 1 INCREMENT BY 1
    MINVALUE 1 MAXVALUE 9223372036854775807 CACHE 1 NO CYCLE;

CREATE SEQUENCE cta_exec_order_config_audit_audit_id_seq AS bigint
    START WITH 1 INCREMENT BY 1
    MINVALUE 1 MAXVALUE 9223372036854775807 CACHE 1 NO CYCLE;

CREATE SEQUENCE cta_publish_fallback_tokens_token_id_seq AS bigint
    START WITH 1 INCREMENT BY 1
    MINVALUE 1 MAXVALUE 9223372036854775807 CACHE 1 NO CYCLE;

CREATE SEQUENCE cta_users_user_id_seq AS bigint
    START WITH 1 INCREMENT BY 1
    MINVALUE 1 MAXVALUE 9223372036854775807 CACHE 1 NO CYCLE;

CREATE TABLE cta_account_strategy_bindings (
    source_id text NOT NULL,
    binding_name text NOT NULL,
    position_strategy_name text NOT NULL,
    order_strategy_name text NOT NULL,
    updated_at_us bigint NOT NULL,
    shares double precision DEFAULT 1 NOT NULL,
    CONSTRAINT cta_account_strategy_bindings_pkey PRIMARY KEY (source_id, binding_name),
    CONSTRAINT cta_account_strategy_bindings_shares_check CHECK (((shares >= (0)::double precision) AND (shares < 'Infinity'::double precision)))
);

CREATE TABLE cta_account_symbol_leverages (
    source_id text NOT NULL,
    symbol text NOT NULL,
    contract_leverage integer NOT NULL,
    updated_at_us bigint NOT NULL,
    CONSTRAINT cta_account_symbol_leverages_contract_leverage_check CHECK (((contract_leverage >= 1) AND (contract_leverage <= 125))),
    CONSTRAINT cta_account_symbol_leverages_pkey PRIMARY KEY (source_id, symbol)
);

CREATE TABLE cta_auth_sessions (
    session_id bigint DEFAULT nextval('cta_auth_sessions_session_id_seq'::regclass) NOT NULL,
    user_id bigint NOT NULL,
    token_hash bytea NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    last_seen_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT cta_auth_sessions_pkey PRIMARY KEY (session_id),
    CONSTRAINT cta_auth_sessions_token_hash_key UNIQUE (token_hash)
);

CREATE TABLE cta_exec_order_config_audit (
    audit_id bigint DEFAULT nextval('cta_exec_order_config_audit_audit_id_seq'::regclass) NOT NULL,
    source_id text NOT NULL,
    strategy_name text NOT NULL,
    client_addr text NOT NULL,
    expected_updated_at_us bigint,
    result_updated_at_us bigint,
    previous_order_parameters jsonb NOT NULL,
    requested_order_parameters jsonb NOT NULL,
    status text NOT NULL,
    error text,
    attempted_at timestamp with time zone DEFAULT now() NOT NULL,
    completed_at timestamp with time zone,
    CONSTRAINT cta_exec_order_config_audit_pkey PRIMARY KEY (audit_id),
    CONSTRAINT cta_exec_order_config_audit_status_check CHECK ((status = ANY (ARRAY['pending'::text, 'applied'::text, 'failed'::text])))
);

CREATE TABLE cta_order_sources (
    source_id text NOT NULL,
    account_label text NOT NULL,
    venue_label text NOT NULL,
    rocksdb_path text NOT NULL,
    enabled boolean DEFAULT true NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    estimated_fee_rate double precision DEFAULT 0.0004 NOT NULL,
    maker_fee_rate double precision DEFAULT 0.0004 NOT NULL,
    taker_fee_rate double precision DEFAULT 0.0004 NOT NULL,
    theoretical_twap_fee_rate double precision DEFAULT 0.0004 NOT NULL,
    CONSTRAINT cta_order_sources_estimated_fee_rate_check CHECK (((estimated_fee_rate > '-Infinity'::double precision) AND (estimated_fee_rate < 'Infinity'::double precision))),
    CONSTRAINT cta_order_sources_maker_fee_rate_check CHECK (((maker_fee_rate > '-Infinity'::double precision) AND (maker_fee_rate < 'Infinity'::double precision))),
    CONSTRAINT cta_order_sources_pkey PRIMARY KEY (source_id),
    CONSTRAINT cta_order_sources_taker_fee_rate_check CHECK (((taker_fee_rate > '-Infinity'::double precision) AND (taker_fee_rate < 'Infinity'::double precision))),
    CONSTRAINT cta_order_sources_theoretical_twap_fee_rate_check CHECK (((theoretical_twap_fee_rate > '-Infinity'::double precision) AND (theoretical_twap_fee_rate < 'Infinity'::double precision)))
);

CREATE TABLE cta_order_strategies (
    strategy_name text NOT NULL,
    single_order_usdt double precision NOT NULL,
    orders_per_batch integer NOT NULL,
    maker_price_anchor text NOT NULL,
    tick_spacing integer NOT NULL,
    batch_interval_ms integer NOT NULL,
    maker_timeout_ms integer NOT NULL,
    max_maker_requotes integer NOT NULL,
    target_tolerance_usdt double precision NOT NULL,
    updated_at_us bigint NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    max_batch integer DEFAULT 20 NOT NULL,
    algorithm text DEFAULT 'batch'::text NOT NULL,
    pov jsonb DEFAULT '{"liquidity": "maker_then_taker", "duration_ms": 3600000, "limit_price": null, "max_batch_usdt": 300.0, "max_carry_usdt": 600.0, "quote_stale_ms": 1000, "volume_stale_ms": 5000, "participation_rate": 0.1}'::jsonb NOT NULL,
    chase jsonb DEFAULT '{"max_batch": 4, "batch_floor_usdt": 100.0, "max_open_batches": 2, "maker_timeout_sec": 120, "target_tolerance_usdt": 10.0, "maker_amend_cooldown_ms": 1000, "maker_recenter_trigger_bps": 5.0, "strategy_order_rate_limit_10s": 0, "strategy_order_rate_limit_per_min": 0}'::jsonb NOT NULL,
    signal_execution_enabled boolean DEFAULT true NOT NULL,
    CONSTRAINT cta_order_strategies_algorithm_check CHECK ((algorithm = ANY (ARRAY['batch'::text, 'pov'::text, 'chase'::text]))),
    CONSTRAINT cta_order_strategies_batch_interval_ms_check CHECK ((batch_interval_ms >= 0)),
    CONSTRAINT cta_order_strategies_chase_check CHECK ((jsonb_typeof(chase) = 'object'::text)),
    CONSTRAINT cta_order_strategies_maker_timeout_ms_check CHECK ((maker_timeout_ms > 0)),
    CONSTRAINT cta_order_strategies_max_batch_check CHECK ((max_batch > 0)),
    CONSTRAINT cta_order_strategies_max_maker_requotes_check CHECK ((max_maker_requotes >= 0)),
    CONSTRAINT cta_order_strategies_orders_per_batch_check CHECK ((orders_per_batch > 0)),
    CONSTRAINT cta_order_strategies_pkey PRIMARY KEY (strategy_name),
    CONSTRAINT cta_order_strategies_pov_check CHECK ((jsonb_typeof(pov) = 'object'::text)),
    CONSTRAINT cta_order_strategies_single_order_usdt_check CHECK ((single_order_usdt > (0)::double precision)),
    CONSTRAINT cta_order_strategies_target_tolerance_usdt_check CHECK ((target_tolerance_usdt >= (0)::double precision)),
    CONSTRAINT cta_order_strategies_tick_spacing_check CHECK ((tick_spacing >= 0))
);

CREATE TABLE cta_position_history_daily_checkpoints (
    source_id text NOT NULL,
    day_start_us bigint NOT NULL,
    anchor_fingerprint text NOT NULL,
    fills_recv_end_us bigint DEFAULT 0 NOT NULL,
    positions jsonb NOT NULL,
    completed_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT cta_position_history_daily_checkpoints_day_start_us_check CHECK ((day_start_us >= 0)),
    CONSTRAINT cta_position_history_daily_checkpoints_fills_recv_end_us_check CHECK ((fills_recv_end_us >= 0)),
    CONSTRAINT cta_position_history_daily_checkpoints_pkey PRIMARY KEY (source_id, day_start_us)
);

CREATE TABLE cta_position_history_sources (
    source_id text NOT NULL,
    anchor_fingerprint text NOT NULL,
    effective_anchor_ts_us bigint,
    scanned_recv_ts_us bigint NOT NULL,
    recent_records jsonb DEFAULT '[]'::jsonb NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT cta_position_history_sources_effective_anchor_ts_us_check CHECK ((effective_anchor_ts_us >= 0)),
    CONSTRAINT cta_position_history_sources_pkey PRIMARY KEY (source_id),
    CONSTRAINT cta_position_history_sources_scanned_recv_ts_us_check CHECK ((scanned_recv_ts_us >= 0))
);

CREATE TABLE cta_position_snapshot_entries (
    source_id text NOT NULL,
    snapshot_ts_us bigint NOT NULL,
    symbol text NOT NULL,
    venue_code smallint NOT NULL,
    quantity double precision NOT NULL,
    reference_price double precision,
    CONSTRAINT cta_position_snapshot_entries_pkey PRIMARY KEY (source_id, snapshot_ts_us, symbol, venue_code),
    CONSTRAINT cta_position_snapshot_entries_quantity_check CHECK (((quantity <> (0)::double precision) AND (quantity <> 'NaN'::double precision) AND (abs(quantity) <> 'Infinity'::double precision))),
    CONSTRAINT cta_position_snapshot_entries_reference_price_check CHECK (((reference_price > (0)::double precision) AND (reference_price <> 'NaN'::double precision) AND (reference_price <> 'Infinity'::double precision))),
    CONSTRAINT cta_position_snapshot_entries_symbol_check CHECK ((length(symbol) > 0)),
    CONSTRAINT cta_position_snapshot_entries_venue_code_check CHECK (((venue_code >= 0) AND (venue_code <= 255)))
);

CREATE TABLE cta_position_snapshots (
    source_id text NOT NULL,
    snapshot_ts_us bigint NOT NULL,
    note text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT cta_position_snapshots_pkey PRIMARY KEY (source_id, snapshot_ts_us),
    CONSTRAINT cta_position_snapshots_snapshot_ts_us_check CHECK ((snapshot_ts_us > 0))
);

CREATE TABLE cta_position_strategies (
    strategy_name text NOT NULL,
    targets jsonb DEFAULT '{}'::jsonb NOT NULL,
    updated_at_us bigint NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    symbol_order_strategy_overrides jsonb DEFAULT '{}'::jsonb NOT NULL,
    created_by_user_id bigint,
    publish_token_hash text,
    open_visibility boolean DEFAULT false NOT NULL,
    CONSTRAINT cta_position_strategies_pkey PRIMARY KEY (strategy_name),
    CONSTRAINT cta_position_strategies_symbol_order_strategy_overrides_check CHECK ((jsonb_typeof(symbol_order_strategy_overrides) = 'object'::text))
);

CREATE TABLE cta_position_strategy_grants (
    strategy_name text NOT NULL,
    user_id bigint NOT NULL,
    access_level text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT cta_position_strategy_grants_access_level_check CHECK ((access_level = ANY (ARRAY['view'::text, 'configure'::text]))),
    CONSTRAINT cta_position_strategy_grants_pkey PRIMARY KEY (strategy_name, user_id)
);

CREATE TABLE cta_position_strategy_managers (
    strategy_name text NOT NULL,
    user_id bigint NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT cta_position_strategy_managers_pkey PRIMARY KEY (strategy_name, user_id)
);

CREATE TABLE cta_publish_fallback_tokens (
    token_id bigint DEFAULT nextval('cta_publish_fallback_tokens_token_id_seq'::regclass) NOT NULL,
    token_hash text NOT NULL,
    note text DEFAULT ''::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT cta_publish_fallback_tokens_pkey PRIMARY KEY (token_id),
    CONSTRAINT cta_publish_fallback_tokens_token_hash_key UNIQUE (token_hash)
);

CREATE TABLE cta_source_symbols (
    source_id text NOT NULL,
    symbol text NOT NULL,
    venue_code smallint NOT NULL,
    venue text NOT NULL,
    first_event_ts_us bigint,
    first_fill_ts_us bigint,
    last_fill_ts_us bigint,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT cta_source_symbols_first_event_ts_us_check CHECK ((first_event_ts_us >= 0)),
    CONSTRAINT cta_source_symbols_first_fill_ts_us_check CHECK ((first_fill_ts_us >= 0)),
    CONSTRAINT cta_source_symbols_last_fill_ts_us_check CHECK ((last_fill_ts_us >= 0)),
    CONSTRAINT cta_source_symbols_pkey PRIMARY KEY (source_id, symbol, venue_code),
    CONSTRAINT cta_source_symbols_venue_code_check CHECK (((venue_code >= 0) AND (venue_code <= 255)))
);

CREATE TABLE cta_strategy_position_snapshot_entries (
    source_id text NOT NULL,
    snapshot_ts_us bigint NOT NULL,
    strategy_name text NOT NULL,
    symbol text NOT NULL,
    venue_code smallint NOT NULL,
    quantity double precision NOT NULL,
    reference_price double precision NOT NULL,
    CONSTRAINT cta_strategy_position_snapshot_entries_pkey PRIMARY KEY (source_id, snapshot_ts_us, strategy_name, symbol, venue_code),
    CONSTRAINT cta_strategy_position_snapshot_entries_quantity_check CHECK (((quantity <> (0)::double precision) AND (quantity <> 'NaN'::double precision) AND (abs(quantity) <> 'Infinity'::double precision))),
    CONSTRAINT cta_strategy_position_snapshot_entries_reference_price_check CHECK (((reference_price > (0)::double precision) AND (reference_price <> 'NaN'::double precision) AND (reference_price <> 'Infinity'::double precision))),
    CONSTRAINT cta_strategy_position_snapshot_entries_strategy_name_check CHECK ((length(strategy_name) > 0)),
    CONSTRAINT cta_strategy_position_snapshot_entries_symbol_check CHECK ((length(symbol) > 0)),
    CONSTRAINT cta_strategy_position_snapshot_entries_venue_code_check CHECK (((venue_code >= 0) AND (venue_code <= 255)))
);

CREATE TABLE cta_strategy_position_snapshots (
    source_id text NOT NULL,
    snapshot_ts_us bigint NOT NULL,
    note text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT cta_strategy_position_snapshots_pkey PRIMARY KEY (source_id, snapshot_ts_us),
    CONSTRAINT cta_strategy_position_snapshots_snapshot_ts_us_check CHECK ((snapshot_ts_us > 0))
);

CREATE TABLE cta_user_source_permissions (
    user_id bigint NOT NULL,
    source_id text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    access_level text DEFAULT 'configure'::text NOT NULL,
    CONSTRAINT cta_user_source_permissions_access_level_check CHECK ((access_level = ANY (ARRAY['view'::text, 'configure'::text]))),
    CONSTRAINT cta_user_source_permissions_pkey PRIMARY KEY (user_id, source_id)
);

CREATE TABLE cta_users (
    user_id bigint DEFAULT nextval('cta_users_user_id_seq'::regclass) NOT NULL,
    username text NOT NULL,
    password_hash text NOT NULL,
    role text DEFAULT 'user'::text NOT NULL,
    disabled boolean DEFAULT false NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT cta_users_pkey PRIMARY KEY (user_id),
    CONSTRAINT cta_users_role_check CHECK ((role = ANY (ARRAY['admin'::text, 'user'::text])))
);

ALTER TABLE cta_account_strategy_bindings ADD CONSTRAINT cta_account_strategy_bindings_order_strategy_name_fkey
    FOREIGN KEY (order_strategy_name) REFERENCES cta_order_strategies(strategy_name);

ALTER TABLE cta_account_strategy_bindings ADD CONSTRAINT cta_account_strategy_bindings_position_strategy_name_fkey
    FOREIGN KEY (position_strategy_name) REFERENCES cta_position_strategies(strategy_name);

ALTER TABLE cta_account_strategy_bindings ADD CONSTRAINT cta_account_strategy_bindings_source_id_fkey
    FOREIGN KEY (source_id) REFERENCES cta_order_sources(source_id);

ALTER TABLE cta_account_symbol_leverages ADD CONSTRAINT cta_account_symbol_leverages_source_id_fkey
    FOREIGN KEY (source_id) REFERENCES cta_order_sources(source_id);

ALTER TABLE cta_auth_sessions ADD CONSTRAINT cta_auth_sessions_user_id_fkey
    FOREIGN KEY (user_id) REFERENCES cta_users(user_id) ON DELETE CASCADE;

ALTER TABLE cta_exec_order_config_audit ADD CONSTRAINT cta_exec_order_config_audit_source_id_fkey
    FOREIGN KEY (source_id) REFERENCES cta_order_sources(source_id);

ALTER TABLE cta_position_history_daily_checkpoints ADD CONSTRAINT cta_position_history_daily_checkpoints_source_id_fkey
    FOREIGN KEY (source_id) REFERENCES cta_order_sources(source_id);

ALTER TABLE cta_position_history_sources ADD CONSTRAINT cta_position_history_sources_source_id_fkey
    FOREIGN KEY (source_id) REFERENCES cta_order_sources(source_id);

ALTER TABLE cta_position_snapshot_entries ADD CONSTRAINT cta_position_snapshot_entries_source_id_snapshot_ts_us_fkey
    FOREIGN KEY (source_id, snapshot_ts_us) REFERENCES cta_position_snapshots(source_id, snapshot_ts_us) ON DELETE CASCADE;

ALTER TABLE cta_position_snapshots ADD CONSTRAINT cta_position_snapshots_source_id_fkey
    FOREIGN KEY (source_id) REFERENCES cta_order_sources(source_id);

ALTER TABLE cta_position_strategies ADD CONSTRAINT cta_position_strategies_created_by_user_id_fkey
    FOREIGN KEY (created_by_user_id) REFERENCES cta_users(user_id) ON DELETE SET NULL;

ALTER TABLE cta_position_strategy_grants ADD CONSTRAINT cta_position_strategy_grants_strategy_name_fkey
    FOREIGN KEY (strategy_name) REFERENCES cta_position_strategies(strategy_name) ON DELETE CASCADE;

ALTER TABLE cta_position_strategy_grants ADD CONSTRAINT cta_position_strategy_grants_user_id_fkey
    FOREIGN KEY (user_id) REFERENCES cta_users(user_id) ON DELETE CASCADE;

ALTER TABLE cta_position_strategy_managers ADD CONSTRAINT cta_position_strategy_managers_strategy_name_fkey
    FOREIGN KEY (strategy_name) REFERENCES cta_position_strategies(strategy_name) ON DELETE CASCADE;

ALTER TABLE cta_position_strategy_managers ADD CONSTRAINT cta_position_strategy_managers_user_id_fkey
    FOREIGN KEY (user_id) REFERENCES cta_users(user_id) ON DELETE CASCADE;

ALTER TABLE cta_source_symbols ADD CONSTRAINT cta_source_symbols_source_id_fkey
    FOREIGN KEY (source_id) REFERENCES cta_order_sources(source_id);

ALTER TABLE cta_strategy_position_snapshot_entries ADD CONSTRAINT cta_strategy_position_snapshot_en_source_id_snapshot_ts_us_fkey
    FOREIGN KEY (source_id, snapshot_ts_us) REFERENCES cta_strategy_position_snapshots(source_id, snapshot_ts_us) ON DELETE CASCADE;

ALTER TABLE cta_strategy_position_snapshots ADD CONSTRAINT cta_strategy_position_snapshots_source_id_fkey
    FOREIGN KEY (source_id) REFERENCES cta_order_sources(source_id);

ALTER TABLE cta_user_source_permissions ADD CONSTRAINT cta_user_source_permissions_source_id_fkey
    FOREIGN KEY (source_id) REFERENCES cta_order_sources(source_id) ON DELETE CASCADE;

ALTER TABLE cta_user_source_permissions ADD CONSTRAINT cta_user_source_permissions_user_id_fkey
    FOREIGN KEY (user_id) REFERENCES cta_users(user_id) ON DELETE CASCADE;

ALTER SEQUENCE cta_auth_sessions_session_id_seq OWNED BY cta_auth_sessions.session_id;
ALTER SEQUENCE cta_exec_order_config_audit_audit_id_seq OWNED BY cta_exec_order_config_audit.audit_id;
ALTER SEQUENCE cta_publish_fallback_tokens_token_id_seq OWNED BY cta_publish_fallback_tokens.token_id;
ALTER SEQUENCE cta_users_user_id_seq OWNED BY cta_users.user_id;

CREATE INDEX cta_account_strategy_bindings_order_idx ON public.cta_account_strategy_bindings USING btree (order_strategy_name);
CREATE INDEX cta_account_strategy_bindings_position_idx ON public.cta_account_strategy_bindings USING btree (position_strategy_name);
CREATE INDEX cta_auth_sessions_expiry_idx ON public.cta_auth_sessions USING btree (expires_at);
CREATE INDEX cta_auth_sessions_user_idx ON public.cta_auth_sessions USING btree (user_id);
CREATE INDEX cta_exec_order_config_audit_source_time_idx ON public.cta_exec_order_config_audit USING btree (source_id, attempted_at DESC);
CREATE INDEX cta_position_snapshots_source_latest_idx ON public.cta_position_snapshots USING btree (source_id, snapshot_ts_us DESC);
CREATE INDEX cta_strategy_position_snapshots_source_latest_idx ON public.cta_strategy_position_snapshots USING btree (source_id, snapshot_ts_us DESC);
CREATE UNIQUE INDEX cta_users_username_lower_idx ON public.cta_users USING btree (lower(username));

-- Virtual accounts own configuration only; they are never Exec order sources.
CREATE TABLE cta_virtual_accounts (
    virtual_id text PRIMARY KEY,
    name text NOT NULL CHECK (length(btrim(name)) > 0 AND octet_length(name) <= 200),
    created_by_user_id bigint REFERENCES cta_users(user_id) ON DELETE SET NULL,
    updated_at_us bigint NOT NULL
);
CREATE TABLE cta_virtual_account_managers (
    virtual_id text NOT NULL REFERENCES cta_virtual_accounts(virtual_id) ON DELETE CASCADE,
    user_id bigint NOT NULL REFERENCES cta_users(user_id) ON DELETE CASCADE,
    PRIMARY KEY (virtual_id, user_id)
);
CREATE TABLE cta_virtual_account_bindings (
    virtual_id text NOT NULL REFERENCES cta_virtual_accounts(virtual_id) ON DELETE CASCADE,
    binding_name text NOT NULL,
    position_strategy_name text NOT NULL REFERENCES cta_position_strategies(strategy_name),
    order_strategy_name text NOT NULL REFERENCES cta_order_strategies(strategy_name),
    shares double precision NOT NULL CHECK (shares >= 0 AND shares < 'Infinity'::double precision),
    PRIMARY KEY (virtual_id, binding_name)
);
CREATE TABLE cta_account_follows (
    source_id text PRIMARY KEY REFERENCES cta_order_sources(source_id),
    virtual_id text NOT NULL REFERENCES cta_virtual_accounts(virtual_id),
    multiplier double precision NOT NULL CHECK (multiplier >= 0 AND multiplier < 'Infinity'::double precision),
    updated_at_us bigint NOT NULL
);
CREATE INDEX cta_account_follows_virtual_id_idx ON cta_account_follows(virtual_id);
-- Durable delivery status, not a second order/target history.
CREATE TABLE cta_follow_publish_queue (
    source_id text NOT NULL,
    binding_name text NOT NULL,
    revision bigint NOT NULL,
    archived boolean NOT NULL DEFAULT false,
    error text,
    PRIMARY KEY (source_id, binding_name),
    FOREIGN KEY (source_id, binding_name) REFERENCES cta_account_strategy_bindings(source_id, binding_name) ON DELETE CASCADE
);
