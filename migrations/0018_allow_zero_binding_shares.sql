ALTER TABLE cta_account_strategy_bindings
    DROP CONSTRAINT IF EXISTS cta_account_strategy_bindings_shares_check;

ALTER TABLE cta_account_strategy_bindings
    ADD CONSTRAINT cta_account_strategy_bindings_shares_check CHECK (
        shares >= 0
        AND shares < 'Infinity'::double precision
    );
