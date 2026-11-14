# jp-meta RapidX monitoring replacement — 2026-11-14 UTC

The account monitor now keeps live balances, positions, margin and risk in memory
and the source's existing account IPC stream. Manager no longer reads
`latest_financial.json` or raw account-message JSONL at runtime. Executions,
statements and Maker/Taker evidence belong to each source's Exec
`persist_manager`; Manager opens those stores read-only.

The monitor retains only a bounded durable delivery outbox, recovery checkpoints,
known asset/position identities and explicit legacy-import cursors. Facts are
synchronized before sending, retired only by an exact durable ACK, and retried
after restart. A full queue applies backpressure rather than dropping facts.
Periodic financial frames preserve the independent Accounts observation timestamp,
so a repeating frame cannot make stale account data appear fresh. See
`account_overview.md` and `nav_daily_fifo_design.md` for the reader contracts, and
the Exec repository's living `docs/rapidx_integration.md` for the owner/outbox and
import procedures.

## Published artifacts and cutover

Exec production branch `arbmm` commits `bba62aa4` and `585a1d7d` were pushed.
The latter fixes resolution of a newly created delivery directory before its
first durable write. Release artifacts were built locally, uploaded to explicit
staging paths, SHA-256 checked and atomically installed. Each persistence owner
was replaced before its account monitor, using only environment-local wrappers.
Trading, Viz and Config were not restarted.

| Exec environment | Portfolio | persist_manager PID | Monitor PID | CPU mask |
| --- | --- | --- | --- | --- |
| binance_exec_rx01 | 2208503406035269 | 376822 | 413434 | 0-5,48-95 |
| binance_exec_rx03 | 2212170733531973 | 388029 | 415832 | 0-5,48-95 |
| binance_exec_rx04_5m05 | 2224101339031877 | 327610 | 364231 | 0-5,48-95 |

SHA-256:

- Monitor: `d1b23e3c666fef3a9939e00dd692bb8478bf8f8493290c590f04a646b14dd2a7`.
- Persistence owner: `5ea37317b7db3f7e20f1e86e51ad70695a6014001cfdda17d5e5b0bbb0b50577`.
- Manager cta_web: `fd55ec8c5b871b756e6fe8fbe7d7794c7e01eb07bbd5948b101655ecdc1bab66`.

For RX04, the first stop wrapper exceeded the caller's 45-second deadline and
removed the supervisor registration while the old monitor still ran. Repeating
the same environment-local wrapper found and stopped the orphan before import
and replacement. The other cutovers used a longer wrapper deadline.

Legacy imports validated 14,033,126 complete rows across first and stopped-tail
passes, then drained all exact ACKs. Imports sent facts through persistence IPC;
neither Manager nor the importer opened an Exec RocksDB for writing. Retained
legacy JSONL totals 13,109,928,015 bytes; its recorded sizes and modification
times stopped changing at cutover. Old snapshots also remain historical evidence.
They must not be deleted as disposable cache or resumed as a second runtime fact
source. Rolling an old persistence owner back requires separate review of new
column families; do not blindly restore an older database copy.

Manager commit `08fad46` was pushed to `origin/main` and deployed only to jp-meta
as `web-releases/20261114T105203Z`. PID 438574 retains CPU 24. HTTP opened at
10:52:38.417539, after startup account-mode and publication preparation. The old
Manager exited normally. The first full NAV build finished at 11:07:38.913651
with eight sources and 4,823,359 fills; later incremental refreshes took about
12 seconds. Cold analysis remained explicitly loading while targets could publish.
The existing frontend, live TOML and Redis service were preserved.

## Verification

- Manager full tests: 535 library tests passed, 17 ignored; default binary tests
  passed. Formatting, check and release builds passed.
- Exec focused tests passed: 88 trade_engine LTP tests, 193 mkt_parsers tests,
  18 persist_common tests and 79 persist_manager runtime library tests. The four
  outbox tests passed again after the absolute-path fix. No CTP code executed.
- A read-only probe using Manager's actual `LiveEquityHub` verified all three
  source namespaces: recovery ready, finite frozen margin, and fresh financial,
  Accounts and equity observations within 45 seconds. It made no HTTP, Redis,
  PostgreSQL or exchange requests.
- All 145 sampled configuration hashes and all four immutable position snapshot
  table fingerprints matched. The table counts remained 5, 177, 4 and 131.
- All 180 sampled long-lived protected process identities and CPU masks matched.
  One additional baseline PID, independent NAV `sync_history` 289921, disappeared.
  Its source implements a finite range command, but its exit status is unavailable;
  successful natural completion is not certified. The original audit retains this
  exception. No NAV process lifecycle or repository changes were performed.
- A 30-second BPF sample counted two fsync calls per new monitor, compared with
  approximately 100/sec in the earlier RX03 sample. These observed frequencies
  differ by about 1500 times; workload windows are not controlled benchmarks.
  Monitor maximum fsync durations were approximately 1.8, 3.8 and 6.6 ms.

Host-only rollback artifacts, imports, archive freeze records, IPC proof and
sanitized process/config/snapshot audits are under
`/home/ubuntu/crypto_cta_manager/backups/rapidx-ipc-20261114/`.

## Redis publication remains a separate unresolved issue

This deployment reduces monitor filesystem pressure; it does not prove all
target publication timeouts are fixed. Three HTTP 502 requests occurred during
the cold build around 10:55. A warm CTA_TOP_V1 request failed at 11:18:35 with
`failed to commit Exec Redis write` after 2001 ms. At 11:55:09, rbf_small failed
for both trade03 and trade04 with the two-second request timeout, reporting
4433 ms wall time. Subsequent requests recovered without a restart. These warm
failures rule out explaining every error solely by cold startup.

Redis 7.4.12 PID 4100946 remains on CPU 28. Its command error counters stayed
`ERR=1`, `NOPERM=1`; the latter is the earlier deliberate ACL check. The cumulative
delayed-fsync count stayed three, with no new slow-fsync notice after 08:55 UTC
at this verification point. A separate ten-minute loopback PING sample made
11,642 successful requests and no errors, but its maximum was 1173 ms. AOF pending
fsync was zero and persistence statuses were healthy in sampled checks.

Redis command histograms showed sub-millisecond SET/GET/EXEC processing buckets,
which do not include every event-loop, filesystem or client scheduling delay.
An initial TCP trace marked server replies at sendmsg return and had a possible
cross-CPU timestamp race; its apparent client-read delays are not causal proof.
A corrected trace marks replies at sendmsg entry. An isolated application-runtime
stall experiment passed with the existing client, so it did not justify moving
the connection driver to another runtime. The exact remaining timeout cause is
not established; no speculative connection-model change was deployed.

The corrected trace subsequently captured a 1730 ms Redis background fdatasync
at 12:19:20 and a 604 ms request-to-server-send gap at 12:18:49. Neither is
time-correlated proof of the earlier failed requests, but they establish that
long storage/server pauses remain possible after removing raw monitor journals.
No new Redis delayed-fsync log does not imply every fsync is fast.

At 12:22 UTC, a repeat IPC probe passed for all three sources, retained legacy
file sizes/mtimes still matched, runtime binary hashes matched and Manager had
zero restarts since cutover. Gateway target requests since 10:52:38 comprised
12,203 HTTP 200 and five HTTP 502; the latest failure remained 11:55:09. The
configuration, immutable snapshot and 180 long-lived-service checks passed again,
with only the explicitly retained finite-task exception above.

## Follow-up: CPU starvation delays the shared filesystem journal

Read-only BPF observation from approximately 12:32–12:51 established a concrete
cause of slow Redis fsync: the filesystem journal and its active writers lack
CPU time on the overloaded housekeeping pool. This is not evidence of a new
attack or a Redis command error. No trading process, CPU binding, Redis setting
or database was changed for this investigation.

The host has only CPUs 0–47 online. CPUs 48–95 are offline SMT siblings, so a
configured mask `0-5,48-95` gives a process only six usable CPUs. Boot isolates
6–47 and puts default IRQ affinity on 0–5. The root ext4 filesystem, including
Redis AOF, Manager and Exec files, is on the same EBS-backed partition.
`jbd2/nvme0n1p1-8` PID845 is restricted to 0–5; Redis and its AOF worker retain
CPU28. All six housekeeping CPUs were approximately fully occupied in the CPU
sample, while CPU28 was about 97% idle.

Observed causal paths:

| UTC | Observation | Measured delay |
| --- | --- | --- |
| 12:35:01 | Redis fdatasync / its `jbd2_log_wait_commit` / file-data wait | 711002 / 708371 / 2610 µs |
| 12:38:24 | RX03 exec-pre-trade PID3016803 holds transaction154829981 while updating file metadata; that transaction waits in its lock phase | 1642938 / 1453955 µs |
| 12:44:52 | Redis fdatasync / journal wait / PID845 runnable but waiting for CPU | 1202319 / 1200566 / 1195906 µs |
| 12:47:49 | trade04 exec-pre-trade PID1505836 holds transaction154833895 while writing stderr; its runnable wait / blocked time | 427987 total / 426685 runnable / 1287 blocked µs |
| 12:47:58 | RX01 exec-pre-trade PID3934756 runnable wait | 2748120 µs |

The 12:44:52 sample is particularly direct: almost all of Redis's 1.202-second
sync coincides with the journal thread waiting 1.196 seconds for CPU. Other
writers also block the journal: the stderr sample spent over 99% of its handle
lifetime ready to run, rather than performing disk I/O. The captured stack is
`file_update_time_flags -> ext4_dirty_inode -> __ext4_journal_start_sb` inside
an ordinary buffered write. A thread descheduled with a journal handle prevents
that shared transaction from committing. Pinning only the Redis userspace
threads does not isolate this dependency.

An 8.085-second process CPU sample found Gate FR04 consuming approximately
2.43 cores of the six-core pool: pre_trade 83.7%, dashboard 73.6%, trade_engine 48.9%
and trade_signal 37.2%. The Binance futures IPC proxy used another 65.6%, with
rolling metrics, depth, other execution processes and system work sharing the
remainder. This identifies competing consumers, not a claim that one account
alone caused every stall or that each high-CPU process is functionally broken.
The Manager process on CPU24 also used approximately 99% in this sample, and
its async worker had a separately measured 942559 µs runnable wait. That is an
additional publication-latency risk.

Redis still had 376 keys, no expiring keys, healthy AOF writes and unchanged
ERR1/NOPERM1 counters. No new bound-account publication failure was seen in the
12:30 onward observation at this point. These traces establish the slow-fsync
mechanism, but the earlier 11:18 and 11:55 HTTP502s lack simultaneous kernel traces;
do not claim a complete per-request attribution of those historical failures.

First reconcile the FR polling-mode difference described below before deciding
how much CPU reassignment is necessary. Reserve enough online physical CPU capacity
for housekeeping/journal work, move remaining heavy application consumers out of
the six-core pool as needed, and reduce contention between Manager analysis and
publication. Review current dedicated-core ownership before selecting new
assignments; offline48–95 cannot supply capacity. Do not weaken AOF durability,
raise request timeouts to conceal the issue, or assume another Redis version
isolates the filesystem.

Host-only evidence uses `storage.txt`, `journal-handles.txt`, `journal-cpu.txt`,
`scheduler.txt`, `flow.txt`, `tcp-rootcause.txt`, `ping-rootcause.jsonl` and
`cpu-consumers-effective.json` in the existing backup directory. Initial handle
off-CPU classification in `journal-scheduling.txt` did not separate sleep from
post-wakeup runnable time; use the corrected `journal-cpu.txt` for that split.
The earlier failed pointer-filter prototype is also superseded. Diagnostic
tracers were stopped explicitly after collection; production services were kept
running throughout. An independent 15-minute PING observer completed 17445
requests without a socket error, with a 1402 ms maximum command round trip and
1707 ms maximum sampling wakeup delay. The wakeup delay is recorded separately
so observer scheduling stalls are not mistaken for Redis command execution time.

### FR idle-mode audit after operator clarification

At 13:02 UTC, a read-only audit checked all44 pre_trade, trade_engine,
trade_signal and fr_signal_dashboard processes across11 running FR deployments.
Only Gate FR04's four processes explicitly enabled fast polling. Its host env.sh
has `ENABLE_IPC_FAST_POLL=1`; all four running processes also contain that value,
and trade_engine additionally has lowercase `enable_ipc_fast_poll=1`. Other
deployments explicitly disable it or omit it with the code's disabled default.
Gate FR05 also has an enabled value in its file, but no matching active processes
were found in this audit; a file alone is not evidence of active load.

FR04 startup logs confirm `fast_poll=true`, pre_trade `spin_iters=1024` with
`idle_policy=yield`, and trade_engine `router_idle_spin_iters=64`. In this mode,
Tokio `yield_now` lets other async tasks run but does not impose an OS-thread
sleep or a CPU quota. With fast polling disabled, empty polls sleep for1 ms.
Sleep is an idle policy: sustained useful work can still consume CPU. The
dashboard initializes the same trade_signal market channels and inherits their
polling policy; its one-second HTTP snapshot refresh is not its only activity.

The operator's earlier FR load reduction therefore exists and is reflected in
other live deployments; FR04 currently retains a different mode. Why that
setting was retained is not established. The immediate configuration question
should be resolved before assuming that moving cores is the only remedy.
No env.sh, flag, trading behavior or process lifecycle was changed in this audit.
Sanitized local evidence: `artifacts/redis_rootcause_20261114/fr_poll_modes.json`.
