CREATE TABLE cta_publish_fallback_tokens (
    token_id bigserial PRIMARY KEY,
    token_hash text NOT NULL UNIQUE,
    note text NOT NULL DEFAULT '',
    created_at timestamptz NOT NULL DEFAULT now()
);
