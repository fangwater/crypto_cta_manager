# el01 log retention, 2026-11-16

The operator approved extending the existing 256/16 MiB application-log policy
to all 15 current Exec pre-trade stderr logs, plus journald at 1 GiB / seven
days. This does not authorize rsyslog/auth/btmp changes, Manager replacement,
signal archival activation, database cleanup or trading restarts.

## Applied: application text logs

The explicit 15-file whitelist is recorded in
`deploy/logging/crypto-cta-pmdaemon-logs.service`. The existing user timer still
checks every minute. A selected file exceeding 256 MiB of allocated blocks has
its old prefix reclaimed, preserving at least the latest 16 MiB. This is a
size policy, not a seven-day application-log history guarantee. Newly added
Exec deployments require an explicit whitelist update.

The service ran successfully and the timer is enabled/active. Selected logs
fell from 9,724,149,760 to 605,855,744 allocated bytes, reclaiming **8.49 GiB**.
Available filesystem space increased from 29,551,636,480 to 38,669,889,536
bytes (about **36.0 GiB**); live writes can change subsequent measurements.

Hole punching preserves inodes, logical lengths and live writer offsets.
Recent tail slices remained intact. All 94 protected process identities, the
Manager configuration hash and Manager RocksDB identity were unchanged.
Neither trading nor Manager was restarted. Use `du` to measure allocated
space; `ls -lh` can still show large sparse logical lengths.

Before reclamation, all 15 files were captured in a private sparse tar/gzip
archive on el01. Gzip integrity, the exact member list, SHA-256 and fsync were
checked. Preserve this rollback evidence; unpack historical logs into a
separate inspection directory rather than overwriting live writers' files.

```text
/home/el01/crypto_cta_manager/backups/el01-log-policy-20261116T101932Z/exec-logs.tar.gz
SHA-256: bebacc81534de5b9694eefbc3e6f227c89b646e7599c260937b5602c255c0b09
```

The backup is on el01, not a powerleader copy. Local operational evidence is at:

```text
/mnt/nvme-raid0-28t/fanghaizhou/crypto_cta_manager_archives/el01-log-policy-20261116T101932Z/
```

## Prepared only: journald

The el01 account belongs to `sudo`, but the SSH session has no usable sudo
authentication cache. Noninteractive sudo returns `a password is required`.
Group membership alone does not supply authentication. **The journald policy
has not been installed.** No system logging service was restarted.

`scripts/apply_el01_journald_limits.py` is staged at the same relative path
under `/home/el01/crypto_cta_manager` and passed its source-host preview. Its
default is read-only; `--execute` requires root and the verified el01 machine
identity. To complete the already-authorized operation, authenticate locally:

```bash
ssh -t cta_exec_bridge 'sudo python3 /home/el01/crypto_cta_manager/scripts/apply_el01_journald_limits.py --execute'
```

Enter the sudo password only in the terminal prompt. The script:

1. Checks free backup headroom above a 10 GiB reserve and snapshots application
   process identities, Manager configuration and DB identity.
2. Backs up already-closed journals before native sync/rotation, then backs up
   newly closed journals. It checks gzip, exact member names, full binary
   content hashes and durable archive writes before installing limits.
3. Installs `deploy/logging/90-crypto-cta-log-limits.conf` atomically and rejects
   conflicting effective limits. It restarts only `systemd-journald.service`
   in one operation to preserve systemd's logging stream descriptors, then
   uses native journal vacuuming.
4. Checks journald service health and protected application identities. It
   restores the previous drop-in/absence if installation or verification fails.

Backups and results are private/root-owned under
`/var/backups/crypto-cta-manager-journald/<timestamp>/`. Configuration rollback
does not automatically reinsert vacuumed journals; the verified archives retain
their binary contents for separate inspection. Native journal limits operate
at file granularity, so active files can put measured usage above 1 GiB.
No automatic backup-removal or remote pull schedule is enabled.

## Validation

Nine focused Python tests passed, including actual Linux hole punching with an
open non-append writer, protected recent-tail data, WAL/path/link rejection,
closed binary-journal backup integrity, and conflicting journald settings.
`systemd-analyze verify` passed for the log service and timer. No Rust changed.
