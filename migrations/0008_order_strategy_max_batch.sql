ALTER TABLE cta_order_strategies
    ADD COLUMN max_batch integer NOT NULL DEFAULT 20 CHECK (max_batch > 0);
