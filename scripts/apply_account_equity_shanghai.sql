-- Explicit settlement-boundary replacement for existing Manager databases.
-- Stop cta_web and back up cta_account_equity_daily before running this script.
-- UTC-derived equity cannot be relabeled as Shanghai midnight: rebuild it from NAV.
BEGIN;
LOCK TABLE cta_account_equity_daily IN ACCESS EXCLUSIVE MODE;
DELETE FROM cta_account_equity_daily WHERE boundary_ts_ms % 86400000 = 0;
ALTER TABLE cta_account_equity_daily
    DROP CONSTRAINT cta_account_equity_daily_boundary_ts_ms_check;
ALTER TABLE cta_account_equity_daily
    ADD CONSTRAINT cta_account_equity_daily_boundary_ts_ms_check
    CHECK (boundary_ts_ms >= 0 AND boundary_ts_ms % 86400000 = 57600000);
COMMIT;
