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
records 112 process identities and 49 configuration hashes. Detailed
diagnostics are in the local ignored `artifacts/leverage_alerts_20261116/`.

## Deployment and live verification

Source commit `762a25c` was pushed to `origin/main`. The exact staged binary was
SHA-256 verified and ran against the complete, unchanged host configuration with
`--once --dry-run`: zero faults and one ONEUSDT 4x informational notice. Only
`bin/cta_monitor` was then replaced atomically, and the existing Manager-root
`start_monitor.sh` restarted that process at 02:59:28 UTC (10:59:28 Shanghai).

The new monitor is PID `169681`, with start ticks `999802655` and SHA-256
`46e840cf243a6092960510868b370e24cc3b0c0f2f91d297a8b62dd953ac7c5d`.
Exactly one monitor instance was present. All 96 other protected application
process identities and CPU affinities, all 49 configuration hashes, and the
Manager RocksDB identity were unchanged. In particular, Manager remained PID
`2471569` and trade07 pre-trade remained PID `1252324`.

The initial broad process comparison also included transient PostgreSQL children
and failed when two had exited. A later comparison saw one additional baseline
child exit and two new children. The evidence does not retain the departed
children's precise roles, so it cannot establish their exact causes of exit.
PostgreSQL's master PID `509018`, all six original main/background children, and
every surviving baseline PostgreSQL identity retained their start ticks and
affinities. These deltas are recorded explicitly in `deployment-proof.json`;
this is not a claim that every PostgreSQL PID stayed unchanged.

Logs through 03:08:12 UTC covered approximately 8 minutes 44 seconds after
restart. There was exactly one successful initial ONEUSDT fallback notification
at 02:59:40.989444 UTC, with no further notice, fault or recovery sends and no
send-error log entries. Seven read-only guard samples over the final two minutes
covered three distinct guard updates: no source/symbol errors, ONEUSDT always
4x with a 200,000 USDT cap, and observation ages between 9.9 and 51.2 seconds.
The monitor start identity and executable hash remained unchanged throughout.
This bounded observation supports the deployment check; regression tests cover
the slow-I/O and transient-evidence cases that were not induced on the live host.

The private NVMe incident archive retains `install.json`, `after.json`,
`after-final.json`, `deployment-proof.json`, `live-observation.json` and cleanup
proof. After verifying the original binary inside the NVMe backup again, the
exact temporary remote rollback file
`bin/cta_monitor.rollback.20261116024557Z` was deleted and its absence checked.
The staging path had already been consumed by the atomic replacement. The
verified NVMe original-program/log backup remains available for rollback.

No trading process, Manager, Viz, Redis or PostgreSQL service was restarted.
No exchange leverage, target quantity, account binding, Exec store or live
position-archive format was changed.
