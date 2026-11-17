# jp-meta expired cold-signal discard — 2026-11-17

The operator explicitly no longer needs expired historical cold signals and
requested deletion every ten minutes. On jp-meta, this supersedes the former
requirement to pull a verified backup before deleting a cold segment. Manager
still migrates its oldest expired hot history into bounded, verified PB cold
segments; an independent timer discards those confirmed segments. No network
transfer or Manager restart is involved. el01 and powerleader's existing local
backups were not changed.

## Installed policy

- User timer: `jp-meta-cold-prune.timer`, every wall-clock minute divisible by
  ten, with persistent scheduling. The first recorded next trigger was
  2026-11-17 05:40 UTC / 13:40 Shanghai.
- Worker: `jp-meta-cold-prune.service`, running
  `scripts/prune_cold_signals.py` through the host's existing
  `bin/cta_position_archive cold-list` / `cold-delete` Rust helpers.
- Limits: 32 segments per run, 45-second execution budget, 90-second systemd
  timeout, Nice 19, idle I/O priority, CPU 36. This is separate from Manager's
  unchanged CPU 24 and its minute / 1,000-record migration cap.
- Pinned hot/cold identity: `71c586be-58f6-4cab-966b-e61b7902f5ec`.
  The worker reads only the hot `IDENTITY` file; it never opens the hot DB.
- Source cold root: `/home/ubuntu/crypto_cta_manager/cold-signals`.
  Complete source packages belonging to the retired segments are also removed.
  Ordinary cold signals are deliberately discarded without a new backup.

Only confirmed, unchanged cold segments older than the existing 33-day boundary
and ten-minute execution guard are eligible. The Rust verifier checks actual
contents and acquires maintenance/segment leases, then preserves
`RETAINED_BOUNDARY.json` and durable retirement metadata. Unconfirmed migration
segments, recent signals, current targets, PostgreSQL anchors, factual fills and
Exec order/report/queue stores remain outside the deletion scope. No compaction
is requested.

The wrapper validates config paths, excludes Exec overlaps and pins the source
identity. It has a separate job lock and persists `.discard-pending.json` before
each deletion. A timeout or lost acknowledgement is retried through Rust's
idempotent retirement proof on the next invocation, including cleanup of a
partially removed segment. Preview is the default; destructive execution requires
both `--execute` and `--discard-without-backup`.

## Validation and initial execution

Six Python tests passed, including two integrations using the built Rust binary
and real isolated RocksDB fixtures. Those tests verified preview behavior,
identity/path/retention rejection, unchanged hot file hashes, retained boundary
contents and recovery after a simulated lost deletion acknowledgement.
Both user units passed `systemd-analyze --user verify` locally and remotely.

The first live service invocation removed 26 cold segments / 24,752 records.
All 26 directories were independently confirmed absent; the last retired
segment's carried-inventory checkpoint remained present and there was no pending
discard receipt. Manager continued creating subsequent cold segments normally.
The timer is enabled and active.

Manager PID 902178 did not restart. After cleanup, all 132 protected process
identities, 57 configuration hashes and 11 key SQL tables matched the deployment
baseline. Both fixed historical financial report hashes were unchanged. Health
was `ok`, archive and PG mirror queues were zero, and the cold worker had no error.

Implementation and units:

- [Worker](../scripts/prune_cold_signals.py)
- [Tests](../scripts/test_prune_cold_signals.py)
- [Service](../deploy/position_archive/jp-meta-cold-prune.service)
- [Timer](../deploy/position_archive/jp-meta-cold-prune.timer)

Evidence: `/home/fanghaizhou/artifacts/jp_meta_20261117T053042Z-cold-prune`.
Remote receipts: `/home/ubuntu/crypto_cta_manager/releases/20261117T053042Z-cold-prune`.

## Operations

```bash
systemctl --user list-timers jp-meta-cold-prune.timer
journalctl --user -u jp-meta-cold-prune.service --since today
```

To stop scheduled discard while retaining ordinary Manager operation:

```bash
systemctl --user disable --now jp-meta-cold-prune.timer
```

Already discarded signals are not recoverable unless separately backed up
before deletion. Stopping the timer does not restore them. A manual pull is still
available for segments that have not yet been discarded; it remains explicitly
operator-controlled and uses the pinned public SSH route.
