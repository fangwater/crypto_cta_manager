# el01 storage and log-retention audit, 2026-11-16

Read-only inspection; no new log truncation/deletion, service restart, Manager
deployment or retention-policy activation was performed. Existing scheduled tasks
continued normally. Figures are a live snapshot and can grow during inspection.

## Queue evidence and database usage

All 16 catalogued Exec stores, including the retired prc archive, contain an
empty `order_queue_positions` CF: estimated keys 0, no first/last visible key,
no SST files and zero SST bytes. The queue is not consuming disk space in these
source stores. This conclusion does not rely solely on process absence.

The inspector opened every store read-only, used low CPU/I/O priority and put
its diagnostic RocksDB logs outside the sources in an isolated marked directory.
Only queue CFs were probed for first/last keys; other usage came from metadata.
All database identities, 94 matched protected process identities and the Manager
configuration hash were unchanged. The temporary remote audit directory was
removed after evidence capture; no source DB files were removed.

| Category | Approximate allocated space |
| --- | ---: |
| Manager DB | 26.6 GiB |
| All 16 Exec DBs | 8.34 GiB |
| pmdaemon text logs | 9.27 GiB |
| Visible `/var/log` tree | 6.95 GiB |
| Manager backups | 1.06 GiB |

The filesystem had about 27.85 GiB available at the initial check. `/var/log`
was measured with ordinary user permissions; system-directory traversal was
partially restricted. Allocated directory bytes and active SST metadata sizes
are different measurements and must not be added as independent categories.

Manager active SST data:

- `position_updates`: 21.70 GiB, roughly 9.12 million estimated records.
- Per-symbol/venue CFs: 1.45 GiB; removal requires a separate usage review.
- `klines_1m`: 0.512 GiB.
- `account_observations`: 0.225 GiB; `nav_fifo_daily`: 0.213 GiB.

Across Execs, active SST data consists mainly of `uniform_orders` (2.685 GiB),
`order_updates` (0.755 GiB), `trade_updates` (0.279 GiB), and unmatched order
updates (0.201 GiB). These are factual order/report records, not disposable logs.

Manager has 2.38 GiB and Execs 3.77 GiB of numbered RocksDB WAL `.log` files.
**Never clean these as application logs**, or delete `MANIFEST`, SST or identity
files manually. RocksDB diagnostic `LOG` / `LOG.old.*` files are separate, about
0.62 GiB combined; only reviewed old diagnostic files are candidates for a
separate retention policy.

## Existing log retention

pmdaemon logs are mostly active Exec pre-trade stderr logs: roughly 9.05 GiB;
trade-engine logs are another 0.22 GiB. All significant log blocks were observed
open by running processes. The older-than-seven-days, unobserved-open files had
zero allocated bytes, so stale-file deletion is not the main saving here.
An `-error.log` suffix means stderr, not an ERROR-only severity filter.

The existing user timer `crypto-cta-pmdaemon-logs.timer` is active every minute,
but its service selects only trade06/trade07 pre-trade error logs. Its existing
`maintain_pmdaemon_logs.py` reclaims old blocks when allocated size exceeds
256 MiB, retaining the latest 16 MiB. It uses hole punching with `KEEP_SIZE`,
preserving the opened inode, file size and writer offsets, and refuses symlinks,
foreign owners and multiply linked files. Logical `ls -l` sizes can remain large
after physical blocks are reclaimed. It does not rewrite order databases.

System rsyslog uses weekly rotation, four retained archives, compression and
delayed compression. `logrotate.timer` is active daily. `syslog.1` occupies about
1.72 GiB uncompressed. No explicit journald limits were found; directory blocks
were about 4.0 GiB while `journalctl --disk-usage` reported about 3.4G in its
visible journal set.

Bounded tail samples showed many WARNs, including repeated untradable-symbol
blocks, below-minimum aggregate residuals and Post Only rejection. Keep their
diagnostic meaning; use a separate future change for transition-based or periodic
summary logging, without changing order/risk behavior. No raw credential-bearing
log lines were printed or saved into this document.

## Prepared policy, not applied

An explicit dry run of the existing log-maintenance program selected all 15
current Exec pre-trade error logs with the existing 256/16 MiB limits. Eleven
exceeded the threshold; estimated reclaimable old text was **8.49 GiB**. This
discards old text and retains a size-bounded tail, not seven days of history.
There was no `--execute` flag in this dry run.

Suggested independent system policy: journald at most 1 GiB / seven days;
ordinary syslog daily rotation with seven compressed archives and size checks.
Keep authentication/btmp audit policy separate; do not hole-punch binary journal
or login records. Use the native journald/rsyslog maintenance mechanisms.

Concrete expanded user-service and journald drafts, the exact file whitelist,
dry-run results, complete CF inventory and protected-state evidence are saved
locally under:

```text
/mnt/nvme-raid0-28t/fanghaizhou/crypto_cta_manager_archives/el01-storage-audit-20261116T093820Z/
```

Applying expanded log reclamation or system retention is a separate explicit
operator decision. Old text may be useful for incident review; confirm the
retained tail/archive policy before discarding it. The current task has changed
no production log policy or trading process.
