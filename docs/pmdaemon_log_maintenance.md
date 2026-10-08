# el01 pmdaemon log maintenance

Last updated: 2026-10-08 UTC.

The independent user timer `crypto-cta-pmdaemon-logs.timer` checks these two files
every minute:

- `~/.pmdaemon/logs/exec_pt_binance_exec_trade06-error.log`
- `~/.pmdaemon/logs/exec_pt_binance_exec_trade07-error.log`

When one file occupies more than 256 MiB of **actual allocated disk blocks**, it
reclaims old blocks and keeps at least the last 16 MiB in the original file. The
limit may be exceeded by writes between minute checks. The service has no
dependency on Manager, monitor, Nginx or any Exec unit and never invokes a
supervisor or reads trading credentials/configuration or persistent order data.

The running pmdaemon writers do not use `O_APPEND`. Truncating their logs to zero
leaves their offsets unchanged, so the next write can restore a large apparent
file size with a sparse hole. Automatic maintenance uses Linux
[`FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE`](https://man7.org/linux/man-pages/man2/fallocate.2.html)
to reclaim the old prefix without changing the inode, apparent length, writer
offsets or recent tail. This requires a supporting Linux filesystem; el01's
ext4 filesystem supports the operation. Unsupported operations fail without a
delete/recreate/truncate fallback. Use `du` or `stat` allocated blocks rather than
`ls` apparent size when checking disk use. Reads of reclaimed prefixes yield
zero bytes; view recent lines with `tail`, avoiding full-file scans.

Only explicit single `.log` filenames are accepted. Symlinks, multiple hard
links and files not owned by the current user are rejected. Each file is checked
independently. The tool prints size/status metadata, never log contents. It is
read-only unless `--execute` is supplied:

```bash
python3 ~/crypto_cta_manager/scripts/maintain_pmdaemon_logs.py \
  --file exec_pt_binance_exec_trade06-error.log \
  --file exec_pt_binance_exec_trade07-error.log

systemctl --user status crypto-cta-pmdaemon-logs.timer
journalctl --user -u crypto-cta-pmdaemon-logs.service -n 10 --no-pager
# Stop future reclamation without touching trading processes:
systemctl --user disable --now crypto-cta-pmdaemon-logs.timer
```

The October 8 inspection found approximately 23.0 GiB and 23.6 GiB in trade06
and trade07's `exec-pre-trade` stderr logs. The retained 1 MiB samples cover
06:37–06:42 UTC. About 94% of sampled lines are BatchExec `target activated`
messages and about 97% are INFO. The Exec logger writes ordinary INFO messages
to stderr, which pmdaemon names `-error.log`; the filename does not identify the
severity. The source emits one target-activation INFO per strategy/symbol.
Other sampled messages include untradable-symbol blocks, minimum-order limits,
unavailable POV volume and Redis reload/maintenance messages. These samples show
the recent cause; the previously truncated full histories cannot be classified
retroactively. Manual truncation freed about 46.6 GiB; each 1 MiB diagnostic tail
was retained privately under `~/.pmdaemon/truncate-backups/20261008T064214Z/`.

The timer was installed and enabled at 06:59 UTC. The 07:01 UTC automatic run
finished successfully with both files below the threshold. Four local tests
covered retained data and continued non-append writes, dry-run behavior, sparse
allocation and symlink/hard-link/path rejection. A separate test on el01's actual
filesystem reclaimed old blocks with the original writer still open and verified
the retained tail, inode and subsequent writes. All 58 protected Exec processes
kept their PIDs/start times; Nginx also kept its PID. Manager remained stopped by
operator request. Root filesystem usage remained 44%, with about 53 GiB available.
