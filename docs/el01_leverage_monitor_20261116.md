# el01 leverage-monitor timing and repeated-notice repair, 2026-11-16

The operator reported repeated ONEUSDT 4x informational notices paired with a
recovery from "leverage guard has not updated for 180 seconds". The installed
monitor was the expected notice-only release (`3a5186d5…`); it had not reverted
to the earlier repeating-fallback policy. Exec pre-trade and Manager retained
their previously recorded process identities.

## Causes and behavior

The monitor captured one wall-clock timestamp at the start of an entire poll,
then used it after asynchronous RocksDB scans and Redis/Viz requests. A guard
update received more than five seconds after that old timestamp entered the
"future timestamp" branch, which used the same stale issue key/text as a real
180-second lapse. The worker could therefore be fresh while the monitor claimed
it had stopped updating. Position/snapshot freshness used the same old reference.

Separately, the alert tracker discarded an informational notice's acknowledged
state whenever that notice was absent for one poll. A stale, malformed or
unreadable guard temporarily omitted its fallback notice. When valid evidence
returned, an unchanged 4x setting was treated as newly observed and sent again,
alongside the stale issue's recovery notice.

The repair evaluates each received observation using the current wall clock
after its I/O completes. True future/invalid timestamps now have a distinct
clock issue and wording; actual staleness reports its observed age. The
per-account/symbol informational identity is stable, with the confirmed
leverage level stored as its state token. Successful delivery remains remembered
through missing/unreadable/stale evidence, while a different fallback level
produces one new notice. Failed delivery is still retried. Real worker errors,
margin/cap failures, true staleness and their repeat/recovery behavior remain.

Successful notice sends now log only channel, issue key and kind, giving an
audit of transitions without logging arbitrary errors or webhook credentials.
Tracking remains process-local, so restarting the monitor may send the current
4x state once; it is then deduplicated across later transient evidence gaps.

This changes only the read-only monitor. The existing preferred 5x / permitted
4x and 3x policy, explicit overrides, exchange API bindings and Exec code are
unchanged. No Manager DB migration is involved.

## Evidence and local validation

At the first read, the live guard was about six seconds old, had no source or
per-symbol error, and confirmed ONEUSDT 4x with a 200,000 USDT cap. The sampled
required notional was about 18,934 USDT. Read-only AOF inspection recovered 15
updates from 02:20:20 through 02:34:20 UTC: no recorded guard errors, every ONE
level was 4x, and the maximum update gap was 60.548 seconds. This bounded window
does not reconstruct all earlier DingTalk messages; the original report did not
include its timestamp and the old monitor did not log successful notice sends.

Thirty-eight monitor tests passed. New regressions cover a fresh observation
arriving after a slow poll start, genuine stale/clock failures, repeated
stale/malformed/transport-error recovery without another 4x notice, failed-send
retry across an evidence gap, per-account isolation, and 4x→3x→4x transitions.
Formatting, `cargo check --bin cta_monitor` and its local release build passed.

The original monitor executable and both supervisor logs were streamed to a
private powerleader NVMe archive, with each member hash independently checked:

`/mnt/nvme-raid0-28t/fanghaizhou/crypto_cta_manager_archives/incidents/el01/20261116024557Z-leverage-monitor/`

`before-monitor.tar.gz` preserves only those three explicit files and its
manifest; no environment/credential file was included. The pre-change footprint
records 112 protected process identities and 49 configuration hashes. Detailed
diagnostics are in the local ignored `artifacts/leverage_alerts_20261116/`.

Deployment must first run the exact staged candidate with the complete host
configuration using `--once --dry-run`, then replace only `bin/cta_monitor` and
restart it through the existing Manager-root `start_monitor.sh`. The config,
Manager, Exec, Redis, Viz and other trading processes must retain their baseline
identities/settings. Deployment results are recorded below after verification.
