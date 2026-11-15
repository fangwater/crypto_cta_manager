# el01 dzy_group0807 BTC position alert, 2026-11-15

The reported alert compared configured BTCUSDT quantity `-1.285597100777`
with factual account quantity `-1.282`, with no exchange-live order quantity.
The source is `binance_exec_trade07` / `dzy_group0807`, native Binance USD-M.

The 02:49 UTC Exec Viz snapshot reproduced these quantities. Redis targets and
Exec targets agreed; the seven strategy allocations summed to the factual account
quantity within `1.8e-13 BTC`. This is not an unexplained ownership discrepancy.
The earlier November 12 strategy-ownership correction and the unenabled signal
archive retention are separate from this monitor calculation.

| Component | BTC quantity, signed target minus current | At snapshot mark, USDT magnitude |
| --- | ---: | ---: |
| Whole configured/account gap | -0.003597100777303464 | 232.74519 |
| Four Exec-completed `exchange_minimum` tails | -0.003457869476208852 | 223.73643 |
| Remaining CS01_SK_1H difference | -0.000139231301094611 | 9.00876 |

The four individual tails were approximately 0.000798, 0.000760, 0.000971 and
0.000929 BTC. Each is below the 0.001 BTC minimum reported by Exec; their sum
is nevertheless larger than the monitor's 50 USDT residual threshold.
CS01_SK_1H reported an active batch, zero executable pending/live quantity and
an unfinished completion flag, with its own configured target tolerance 10 USDT.
This investigation did not infer that an unfinished batch was an exchange fill.

The monitor previously applied the USDT dust test to the whole gap, then accepted
completed tails only if they explained the quantity gap exactly within numerical
tolerance. Thus an otherwise acceptable 9 USDT remainder caused this 233 USDT
composite gap to alert. Apply both numerical and USDT residual checks to the
unexplained gap after subtracting only explicitly accepted `exchange_minimum`
and `target_tolerance` residuals. Keep the 50 USDT threshold unchanged. Material
unrelated discrepancies, unknown pricing and other completion reasons still alert.

During observation the factual account moved from `-1.282` to `-1.284 BTC`
after a later target update. At 02:55 UTC, the unexplained gap was approximately
6.25 USDT; strategy quantities continued to sum to the account. This provides
no basis for manually trading the entire composite gap.

## Validation and scope

The exact seven-row frozen case is a regression fixture. It verifies suppression
of this composite gap while retaining alerts for a 5 USDT configured threshold,
an independent larger account discrepancy, absent marks and an unaccepted
completion reason. All 30 monitor unit tests, `cargo fmt --check`,
`cargo check --bin cta_monitor` and the release build passed.

A source07-only private copy of the live configuration was used for
`--once --dry-run` on el01. The new monitor supports the full live configuration
fields and returned `CTA monitor: OK`. Its transient unit had 1 GiB MemoryMax,
25% CPUQuota, nice 19 and a 60-second lifetime bound. No DingTalk credentials
were loaded and no test messages were sent.

The old executable failed to parse the live configuration's later-added
`kline.allow_trading_ip` field. A minimal monitor-only configuration retaining
the same source and monitor settings allowed a separate old-executable dry-run,
which also returned OK after the 03:00 target update. These sequential live
checks are not a frozen-input before/after proof and do not mean the live
process was upgraded. The frozen regression is the evidence for the fix.
A future monitor replacement must use the current executable; directly
restarting the old executable against the current configuration would fail.

Old executable remains installed; only the standalone monitor candidate was built
locally. Candidate SHA-256:
`3b20d0de50b3e41b4411b4383c68ab96f4e02472765a38f19be8a01f080474db`.
Temporary remote candidate/configuration copies were removed after the checks.
Diagnostic logs remain locally under `artifacts/el01_dzy_btc_20261115/`.
No trading target, binding, position ledger, Exec RocksDB, PostgreSQL data,
trading configuration or running executable was changed. No service restarted.
Monitor PID `1151833`, Manager `2471569`, trade07 pre-trade `101017`, engine
`395408`, persistence `95217` and Viz `101840` stayed in place.

## Additional execution observation

The trade07 pre-trade error log was approximately 2.95 GB. A bounded 10-second
sample grew 121,989 bytes and contained 480 repeated BTC exchange-minimum
warnings from strategy ID `55638503`; pre-trade consumed about 24.2% of one CPU
core over that sample. This is a separate execution-state/logging issue requiring
further investigation. Do not change order rules or force a trade to silence it.
No Exec source modifications, binary replacements or restarts were made here.
