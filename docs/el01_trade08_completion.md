# el01 Crypto_cta1 completion-state update

Last verified: 2026-10-12 12:49 UTC.

The operator authorized updating `Crypto_cta1` / `binance_exec_trade08` after
its BTCUSDT mismatch alert. At investigation time the target was
1.4685829902438534 BTC and the reported account position was
1.4700000286102295 BTC. Two strategies had residual sells of approximately
0.000639 and 0.000778 BTC, below the loaded 0.001 BTC minimum quantity.
The 12:02:58 UTC internal cross cleared their completion flags without waking
the parked scheduler in the September 16 executable. Both then reported no
live orders, incomplete execution, and no completion deadline.

Published locally built Exec binaries from synchronized `arbmm` commit
`b8611bd1d61f20e24cfa30a7a0d09a74d5793db0`, which includes the internal-cross
wakeup fix `9d4a5fe0` and the subsequent completion fixes. The account's
`exec-pre-trade`, `trade_engine`, `account_monitor`, `persist_manager`, and
`viz_server` were replaced together because their IPC contracts changed since
September. Updated the three component start wrappers and added the explicit
`binance-futures` venue to its Viz configuration. Used only environment-local
stop/start scripts. Startup performs the existing exchange cancellation flow.
Config and the stopped trade_signal process were not restarted or started.

Updated only Manager's standalone `cta_monitor` binary. Completed residuals are
now computed as `target_qty - current_qty`; the newer Exec `pending_qty` is
executable work and becomes zero at completion. Suppression still requires
explicit `exchange_minimum` / `target_tolerance` completion and residuals that
explain the account-level difference. An unrelated account discrepancy remains
an alert. The shared checkout's pre-existing edits were preserved; Manager's
web binary, schema and frontend were not deployed.

Validation:

- 61 BatchExec tests, 57 account/order/Viz contract tests and 28 Manager monitor
  tests passed; formatting and monitor checking passed; release builds passed.
- Uploaded and running SHA-256 hashes matched all five Exec artifacts and the
  monitor artifact. Runtime library and Viz configuration preflights passed.
- Authenticated Nginx requests to the account's Viz page, snapshot and Config
  returned HTTP 200; its WebSocket upgraded with HTTP 101. Temporary validation
  sessions were deleted.
- Before deployment the new monitor's dry run reproduced exactly the trade08
  BTC alert. After deployment dry runs returned `CTA monitor: OK`.
- BTC account position is now reported as 1.47. The two affected strategy rows
  report `execution_complete=true`, `completion_reason=exchange_minimum`,
  `pending_qty=0`, and no live orders. The residual itself remains factual;
  no forced trade was submitted to erase a below-minimum strategy residual.
- All 81 protected runtime process identities stayed unchanged. All 14 trading
  TOML hashes, Manager TOML and Nginx configuration hashes stayed unchanged.
  PostgreSQL connection workers are transient and excluded from the runtime
  identity check; no PostgreSQL service operation was performed.

The remote artifact manifest is
`/home/el01/binance_exec_trade08/COMPLETION-RELEASE.json`.
At the operator's request, the previous binaries, scripts and Viz configuration
backup was deleted after verification. The exact directory
`/home/el01/binance_exec_trade08/backups/completion_20261012T1245Z`
is confirmed absent; current binary and release-manifest hashes are unchanged.
Deployment evidence and the exact monitor patch are in the local ignored
`artifacts/trade08_completion_20261012/` directory.
