UPDATE cta_account_strategy_bindings AS binding
SET shares = binding.shares * settings.leverage
FROM cta_account_settings AS settings
WHERE settings.source_id = binding.source_id;

ALTER TABLE cta_position_strategies
    DROP COLUMN equity_usdt;

DROP TABLE cta_account_settings;
