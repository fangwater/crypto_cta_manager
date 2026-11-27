-- Explicit maintenance for a domestic Manager with an existing attribution ledger.
-- Admits the 恢复 member event (strategy_resume). Held targets of stopped members
-- live inside the portfolio JSON state; no table or row is rewritten.
BEGIN;
ALTER TABLE cta_native_netting_events
    DROP CONSTRAINT cta_native_netting_events_kind_check,
    ADD CONSTRAINT cta_native_netting_events_kind_check
        CHECK (kind IN ('initialize', 'target_batch', 'exchange_fill_allocation',
                        'external_demand_release', 'scope_extension',
                        'strategy_stop', 'strategy_resume', 'strategy_retire'));
COMMIT;
