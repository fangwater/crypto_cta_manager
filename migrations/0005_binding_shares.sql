ALTER TABLE cta_account_strategy_bindings
    ADD COLUMN shares double precision NOT NULL DEFAULT 1 CHECK (shares > 0);
