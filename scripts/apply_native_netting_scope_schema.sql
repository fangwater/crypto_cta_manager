-- Explicit maintenance for a domestic Manager with an existing attribution ledger.
BEGIN;
ALTER TABLE cta_native_netting_events
    DROP CONSTRAINT cta_native_netting_events_kind_check,
    ADD CONSTRAINT cta_native_netting_events_kind_check
        CHECK (kind IN ('initialize', 'target_batch', 'exchange_fill_allocation',
                        'external_demand_release', 'scope_extension'));
COMMIT;
