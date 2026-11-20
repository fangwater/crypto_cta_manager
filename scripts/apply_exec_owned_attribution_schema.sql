-- Exec owns the domestic strategy ledger (crypto-aligned). Manager no longer
-- freezes external demand, so the per-batch demand budget table is retired.
-- Immutable batch receipts remain in cta_native_netting_events.
BEGIN;
DROP TABLE IF EXISTS cta_native_netting_demands;
COMMIT;
