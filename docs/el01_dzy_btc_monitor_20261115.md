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

## Repair deployed, 2026-11-15

After the operator requested all three program issues be fixed, the release
monitor `3b20d0de50b3e41b4411b4383c68ab96f4e02472765a38f19be8a01f080474db`
was validated against the complete live configuration under an isolated,
resource-bounded `--once --dry-run` process; it returned OK. It was atomically
installed and restarted using `start_monitor.sh` at 03:23:30 UTC. The old process
was terminated by the wrapper's leak cleanup. New monitor PID: `249186`.
The live TOML was retained byte-for-byte (SHA-256
`4413ad6a8fc125bac176d38b53cfb0d28c108fc8b4a5ab77053a4b7940f47e64`).
No parser downgrade or configuration-field removal was used in production.

In the independent Exec project, production branch `arbmm` commit `36498455`
separates the exchange-minimum warning budget from retry scheduling. It emits
at most once per strategy per 60 seconds and reports suppressed checks. The
release also includes the existing `cc742676` idle-batch settlement behavior;
unresolved children and orphans are retained until terminal evidence arrives.
Eighty-six BatchExec tests and all 1,578 runnable library tests passed, with
3 existing ignored tests. The release build and formatting/checks passed.

Before replacing trade07 pre-trade, signed read-only queries explicitly bound
`154.197.32.6` with proxies disabled verified STANDARD Multi-Assets Mode and
observed its open UM orders. Market rules and its current position ledger were
also read and saved. The native environment-local startup script stopped the
old pre-trade, validated account mode, cancelled only its USD-M open orders and
confirmed an empty startup scope, then resumed the saved targets. Gate passed
at 03:32:54 UTC, positions became ready, and no manual order or target change
was submitted. New pre-trade PID: `385875` (old `101017`).

Exec binary SHA-256:
`9d71a9acf4188e6f97ce83b1455cfc3356999f8e0eef3192307680c4a0b38bc6`.
Monitor and pre-trade are the only two changed process identities among the
94 captured live supervisor/Manager identities; the other 92 retain PID,
start ticks, executable identity and CPU bindings. The production Manager,
trade engine, persistence, Viz, configuration gateways and other accounts
were not restarted. Exec receipt:
`/home/el01/binance_exec_trade07/EXEC-RESIDUAL-RELEASE.json`.

Across a 283.9-second post-deployment observation spanning another target update,
there were only 6 minimum-quantity warnings across two strategies, each obeying
the 60-second budget. BTC had zero active batches, every BTC row was completed,
and the unexplained account difference was approximately `1.8e-13 BTC`.
All 151 represented symbols passed strategy/account quantity additivity.
The sampled pre-trade load was 20.64% of one core, compared with the earlier
24.2% sample; these different workload windows are not a controlled benchmark.

The initial 2.95 GB log figure was logical file length, not allocated space.
Inspection found an already active `crypto-cta-pmdaemon-logs.timer`, retaining
the recent 16 MiB when actual allocated blocks exceed 256 MiB. Its existing
trade06/trade07 scope and settings were preserved. Before restart, trade07's
log occupied about 190 MB of disk blocks; the new log occupied about 1.9 MB
at observation. No extra cleanup timer was installed and no log was manually
truncated. Supervisor replacement reopened its own log files normally.

Before any process stop, old executables and current log contents were streamed
to powerleader NVMe and gzip/tar and executable hashes were verified. A final
16 MiB log-tail snapshot was also captured and verified immediately before the
pre-trade replacement. Backup directory, mode 0700:

`/mnt/nvme-raid0-28t/fanghaizhou/crypto_cta_manager_archives/incidents/el01/20261115-dzy-btc/`

It contains `runtime-before.tar.gz` (36,127,464 compressed bytes), its per-file
manifest and `pretrade-final-tail.tar.gz`. No credential file was included.
Local detailed verification remains under `artifacts/el01_dzy_fix_20261115/`.

## Separate ONEUSDT exchange limit found during verification

Both old and new pre-trade logs contain Binance `-2027` rejections. The preserved
pre-restart tail alone contains 2,342 such messages, confirming this preceded
the replacement. The failing symbol is ONEUSDT, involving CS01_SK_1H and
rbf_small; invalid attempts also consume the source's order-rate budget.

Read-only signed position/risk/bracket/account queries via the same fixed IP found:

- ONE leverage: 5x; maximum notional: 50,000 USDT.
- Factual ONE notional: approximately 51,631 USDT, already beyond that cap.
- Published aggregate target: approximately 55,194 USDT, leaving about
  3,563 USDT unexecuted at that observation.
- The published exchange bracket for 4x permits 200,000 USDT; 3x permits
  500,000 USDT. The least leverage change needed for this target is 5x to 4x.
- At this target, 4x would require approximately 2,760 USDT more initial margin
  than 5x. The signed account response showed about 1,585,606 USDT available.

The operator approved changing only ONEUSDT to 4x. At 04:05:28 UTC the signed
POST and a separate signed position-risk readback confirmed 4x and a 200,000
USDT cap, bound to the unchanged trading IP. Both blocked strategy targets then
completed. The last -2027 in the retained trade-engine/pre-trade tail was at
04:05:27 UTC. No target quantity, account mode or egress was changed.

## Automatic leverage fallback, deployed 2026-11-15

The operator subsequently authorized preferred 5x with 4x and 3x fallback when
setting leverage fails or position limits prevent execution. Exec production
commit `2ff5b0ad` implements the opt-in native STANDARD USD-M policy; Manager
commit `6d984dd` reads its diagnostics and routes notices to the existing order
DingTalk channel. Only `binance_exec_trade07` / dzy_group0807 has
`EXEC_BINANCE_UM_LEVERAGE_FALLBACK=1`. Other deployments remain opt-out.

New symbols first attempt 5x. Only explicit HTTP 400 Binance -4028 or -2027
permits lower candidates. A separate 60-second Exec worker checks original
aggregate targets, factual holdings and complete open-order peak exposure;
BatchExec/ChaseExec also record actual -2027 rejections for boundary cases.
Account-specific brackets determine the highest feasible lower level. Fresh
holdings and available collateral are checked before each mutation, including
initial margin for the complete exposure and a 1% margin reserve. POST responses
require signed readback; an uncertain result cannot trigger another lower
mutation until reconciled. Network/auth/rate/margin errors, incomplete evidence,
and inadequate 3x capacity remain visible as blocked conditions. Never lower
below 3x, resize a target, or automatically raise a successful fallback to 5x.
Explicit per-symbol manual overrides exclude their symbols from automation.
See Exec's living `docs/exec_leverage_policy.md` for the complete policy.

Exchange clients bind the current trading IP with proxies and redirects
disabled. The worker stops checks if that configured IP changes, pending a
restart/revalidation. It runs independently of normal target publication and
holds no Manager publication/reloader lock during periodic REST work. Existing
new-symbol initialization retains its activation gate. Manager's monitor stays
read-only; it does not set leverage, mutate Redis or submit/cancel orders.

Exec publishes one current source/venue-scoped Redis diagnostic document at
`binance_exec_trade07:binance-futures:exec_leverage_guard`. The monitor reports
4x/3x settings, blocked initialization/adjustments and source-level query errors;
updates older than 180 seconds raise a stale-worker alarm. Existing transition,
repeat, recovery and pending failed-send retries apply. A successful lower
setting remains a monitored condition and is not sent on every poll. This is a
risk response with an approximately 60-second detection cycle, not a guarantee
that the first exchange rejection can never occur.

Thirty-one leverage tests passed, including 14 new policy/HTTP fixture tests:
5x success, 5x rejected then 4x, 4x cap insufficient then 3x, rejected 4x then 3x,
insufficient margin, 3x insufficient, auth/rate/network restrictions, open-order
peaks, wrong/partial identities, lost POST responses and readback mismatch.
The 86 BatchExec, 25 ChaseExec and 49 configuration tests passed. Manager's
complete library suite passed: 545 tests, 17 existing ignored; 32 are monitor
tests, including fallback notices, blocked/stale states and failed-send retry.
Formatting, binary checks and both local release builds passed.

The final signed read-only preflight covered all 946 USD-M risk rows with no
invalid records. It found 103 active/target symbols at 5x and ONE at 4x,
no missing target symbols and no cap breaches; available balance was about
1.571 million USDT. The full-source monitor dry-run returned OK before restart.
The installed binaries were SHA-256 checked and atomically replaced:

- `exec-pre-trade`: `42f2efa27fd76c98902962e79b5a09ed51575e126625e4f036de9756c3181c98`.
- `cta_monitor`: `2261254ad2cd95b7ff176bec2673e69049256b18ec037c85adc14b7cd0fe8a1d`.

Monitor restarted via its wrapper at 04:30:04 UTC, PID `1249221`. Trade07
pre-trade restarted via its environment-local wrapper at 04:30:13 UTC, PID
`1252324`; that wrapper retains its normal startup USD-M cancellation and
account-mode gate. The only env edit is the opt-in export; the original env
is backed up privately on the host, never copied into the repository/NVMe
archive. The pre-replacement binaries and pre-trade logs are verified in
`before-leverage-auto.tar.gz` beside the earlier NVMe incident backups.

First runtime verification found a fresh diagnostic covering 371 indexed/held
symbols, no source/per-symbol errors, and ONE confirmed at 4x / 200,000 USDT.
Its required notional was approximately 56,015 USDT and both target strategies
were complete with zero active batches. All 151 represented symbols passed
strategy/account quantity additivity; no new -2027 appeared. Of the 94 audited
identities only pre-trade and monitor changed; the other 92 and all 15 captured
Manager/trade-engine TOML hashes remained identical. `cta_web` remained PID
`2471569`; trade engine, persistence, Viz and the other accounts were retained.
Detailed runtime verification remains in `artifacts/el01_dzy_fix_20261115/`.

A second runtime sample 106.5 seconds later covered two additional worker polls
(118.9 seconds between diagnostic timestamps). It again had no worker/symbol
errors, no -2027, complete ONE targets, and exact account/strategy additivity
within the documented numeric tolerance. Pre-trade log growth over this interval
was about 82 KB. The post-deployment full-source monitor dry-run detected exactly
one condition: the intended ONEUSDT 4x setting, with no other source faults.
No DingTalk send failures were observed; the dry-run itself sends no messages.
The automatic POST fallback paths were exercised with HTTP fixtures, without
manufacturing rejected live orders or changing another live symbol's leverage.
