-- Explicit maintenance for existing Manager databases. No startup DDL.
BEGIN;
CREATE TABLE cta_account_equity_daily (
    source_id text NOT NULL REFERENCES cta_order_sources(source_id),
    boundary_ts_ms bigint NOT NULL CHECK (boundary_ts_ms >= 0 AND boundary_ts_ms % 86400000 = 0),
    equity double precision NOT NULL CHECK (equity > '-Infinity'::double precision AND equity < 'Infinity'::double precision),
    previous_equity double precision CHECK (previous_equity > '-Infinity'::double precision AND previous_equity < 'Infinity'::double precision),
    nav_change_quote double precision NOT NULL CHECK (nav_change_quote > '-Infinity'::double precision AND nav_change_quote < 'Infinity'::double precision),
    drawdown_15d_percent double precision CHECK (drawdown_15d_percent >= 0 AND drawdown_15d_percent < 'Infinity'::double precision),
    anchor jsonb NOT NULL CHECK (jsonb_typeof(anchor) = 'object'),
    PRIMARY KEY (source_id, boundary_ts_ms)
);
COMMIT;
