CREATE TABLE cta_users (
    user_id bigserial PRIMARY KEY,
    username text NOT NULL,
    password_hash text NOT NULL,
    role text NOT NULL DEFAULT 'user' CHECK (role IN ('admin', 'user')),
    disabled boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX cta_users_username_lower_idx ON cta_users (lower(username));

CREATE TABLE cta_auth_sessions (
    session_id bigserial PRIMARY KEY,
    user_id bigint NOT NULL REFERENCES cta_users(user_id) ON DELETE CASCADE,
    token_hash bytea NOT NULL UNIQUE,
    expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    last_seen_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX cta_auth_sessions_user_idx ON cta_auth_sessions (user_id);
CREATE INDEX cta_auth_sessions_expiry_idx ON cta_auth_sessions (expires_at);

CREATE TABLE cta_user_source_permissions (
    user_id bigint NOT NULL REFERENCES cta_users(user_id) ON DELETE CASCADE,
    source_id text NOT NULL REFERENCES cta_order_sources(source_id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, source_id)
);
