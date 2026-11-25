-- Explicit maintenance for a domestic Manager with an existing attribution ledger.
-- Admits the 停止并清仓 / 删除配置 member events. Stopped and retired members are
-- kept inside the portfolio JSON state; no table or row is rewritten.
BEGIN;
ALTER TABLE cta_native_netting_events
    DROP CONSTRAINT cta_native_netting_events_kind_check,
    ADD CONSTRAINT cta_native_netting_events_kind_check
        CHECK (kind IN ('initialize', 'target_batch', 'exchange_fill_allocation',
                        'external_demand_release', 'scope_extension',
                        'strategy_stop', 'strategy_retire'));
COMMIT;
