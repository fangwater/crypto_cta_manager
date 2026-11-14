# Position signal retention and powerleader backup

Operator request, 2026-11-14: keep online slippage/theoretical inputs for one
month and offload older original signals to powerleader NVMe.

Only Manager's `position_updates` column family is eligible. Exec stores,
current targets, PostgreSQL snapshots, funding, FIFO state and Kline maintenance
are independent. Factual 60D/90D/ALL queries keep their existing history.

The hot boundary is UTC midnight at least 30 days ago, minus ten minutes for
execution schedules and signal matching. Thus 30–31 days plus overlap remain.
An exact MessagePack checkpoint retains each source/binding's last normalized
target and cumulative source/symbol quantities, accumulated in original order
with the theoretical model's existing float tolerance. Restart seeds theoretical
inventory from this checkpoint and scans only the hot suffix. Original JSON
bytes and 12-byte keys are never converted or deduplicated in backups.

Maintenance is opt-in through `[archive].socket`, an absolute Unix socket path
inside a private directory owned by the Manager user. The socket is mode 0600;
there is no public maintenance HTTP API. A dedicated OS thread handles exports
and commits. Publication admission and Redis/IPC never wait for maintenance.
The existing bounded asynchronous archive queue retains its failure semantics.

A seal makes the eligible prefix immutable: late factual enrichment skips it,
and an attempted historical insertion fails closed. Exports stream bounded
64 MiB raw chunks directly over SSH; the live host stores no duplicate export.
The gzip stream contains a magic, a bounded MessagePack header (identity,
range, preceding checkpoint/receipt), raw key/value frames and a SHA-256 receipt.
Keys, message timestamps, ordering, count, byte count, digest, EOF and gzip CRC
are validated. There is only one current internal format.

On powerleader, the puller fsyncs each received file and restores its records
into an independent local backup RocksDB. It reads that range back and verifies
the same digest. Only after the compressed file and receipt are durably renamed
and fsynced does it send the receipt back. Manager independently rescans the
sealed range, verifies it, and atomically saves the new checkpoint/receipt and
deletes that range in one synchronized WAL batch. Retry after a lost reply is
idempotent. A missing, corrupt, stale or mismatched receipt never deletes data.
The identity and receipt chain bind every chunk to one store and ordered prefix.

The powerleader job runs hourly under a local flock. If it is offline, space is
not reclaimed on the live host; originals remain there. The backup root is
`/mnt/nvme-raid0-28t/fanghaizhou/crypto_cta_manager_archives`. This NVMe RAID0
copy is the offloaded durable source, not redundant disaster recovery; replicate
it separately if disk-failure redundancy is required.

Range deletion first releases records logically. An explicit CF-only compaction
can reclaim SST space after the transfer; never manually remove SST/WAL files.
It runs outside archive/publication locks. Compaction can consume disk I/O and
RocksDB background CPU, so run it after the initial drain and monitor publishing.

Deployment and the first pruning pass require an explicitly selected host scope.
Default configuration is disabled. Raw history queries with an archived cursor
return an explicit archived-range response instead of silently omitting records.
Offline originals remain available in the local restored store and gzip chunks.
This is a signal-archive recovery, not a full PostgreSQL/Exec/Manager backup.

## Operation

Build with `cargo build --release --bin cta_web --bin cta_position_archive`.
The normal host deploy script also installs the archive client. Add just this
independent section to each approved live host's existing TOML, substituting its
absolute Manager root, then restart only `crypto-cta-manager-web.service`:

```toml
[archive]
socket = "/absolute/manager/root/archive-control/position.sock"
```

The private directory is created mode 0700, or an existing directory must
already be private. An occupied file, symlink or active socket is rejected.
The maintenance thread lowers its own Linux niceness; trading affinities and
process priorities are unchanged. Sealing refuses any queued target older than
the proposed boundary. Healthy new target writes continue during transfer.

Before enabling, inspect the actual Manager store without changing it:

```bash
cta_position_archive inspect --db /absolute/manager/root/db
```

This prints stable identity, oldest/latest keys, estimated message count and
approximate eligible SST bytes. Estimates exclude unflushed data and are not a
promise of immediate disk reclamation.

Install the released client and puller outside Cargo's disposable build cache:

```bash
install -d -m 0700 /mnt/nvme-raid0-28t/fanghaizhou/crypto_cta_manager_archives/bin
install -m 0700 /mnt/nvme-raid0-28t/fanghaizhou/cargo-target/crypto_cta_manager/release/cta_position_archive /mnt/nvme-raid0-28t/fanghaizhou/crypto_cta_manager_archives/bin/cta_position_archive
install -m 0700 scripts/pull_position_archive.py /mnt/nvme-raid0-28t/fanghaizhou/crypto_cta_manager_archives/bin/pull_position_archive.py
```

Copy `deploy/position_archive/powerleader.example.toml` to
`~/.config/crypto-cta-manager/position-archive.toml`, mode 0600, and enable only
approved hosts. The host label and first observed DB identity are pinned locally;
a replacement live DB or missing local backup chain requires manual review.
A source already pruned cannot silently initialize a new empty backup.

Run a backup-only pass first:

```bash
python3 scripts/pull_position_archive.py --config ~/.config/crypto-cta-manager/position-archive.toml --max-chunks 1
```

Inspect the resulting `host/chunks/<sha256>.json`, gzip and `host/restored/`.
The backup-only pass seals old records but removes none. To complete the verified
pending receipt and continue the initial drain, use `--apply --max-chunks 1000`.
Run `cta_position_archive compact --socket ...` after the initial drain and
measure actual live disk size. Never delete RocksDB files manually.

Install the supplied service/timer in `~/.config/systemd/user/` on powerleader
and run `systemctl --user daemon-reload` then
`systemctl --user enable --now cta-position-archive.timer` after the initial
approved pass. Check `journalctl --user -u cta-position-archive.service` and
`systemctl --user list-timers cta-position-archive.timer`. Each job handles at
most 32 chunks (~2 GiB raw, plus a possible large final record) per host.
Failed hosts do not stop the other host. The restored copy is independent of
the compressed backup; both consume NVMe space, not live-host space.

For offline recovery, create a **new, absent** directory and verify each saved
gzip in receipt-chain order:

```bash
cta_position_archive verify --file /backup/host/chunks/<sha256>.signals.gz --restore /new/offline/archive-db
```

The verifier refuses an existing directory without its own backup marker, a
foreign database identity, conflicting bytes, missing previous receipt or an
incomplete stream. Each receipt is emitted only after WAL sync and readback.
`archive_boundary:*` and `archive_receipt:*` in the backup default CF preserve
initial checkpoint envelopes and the chain. Recovery does not publish targets,
write to PostgreSQL, or open an Exec store for writing.

`/api/health` exposes `position_archive_retained_from_us`; raw-history responses
include `x-cta-archive-retained-from-us`. A cursor below that boundary returns
410. The target-history cache is invalidated after each successful prune and
loads the new checkpoint as one consistent generation. Deployment rollback after
pruning must retain checkpoint-aware code; an older binary that ignores this
metadata cannot reconstruct carried inventory correctly. Disable the local timer
to pause further offload; never erase the checkpoint to roll back.


## Validation and staging, 2026-11-14

Local `cargo fmt --check`, `cargo check --all-targets`, full `cargo test` and
release builds of `cta_web`/`cta_position_archive` passed. Rust: 560 passed,
36 intentionally ignored (including isolated-PostgreSQL/operator diagnostics).
Four Python failure/resume tests passed; systemd units and deployment shell
syntax passed validation. Retention tests use small chunks to exercise repeated
checkpoint advancement across the same-timestamp sequence ordering. Checks cover
corrupt/truncated/trailing streams, foreign identity, conflicting restores,
missing receipt chains, stale receipts, retry after lost confirmation,
late enrichment, current-target/head recovery, CF isolation and compaction.

Full versus pruned/cold-restarted replay has exactly identical selected deltas,
carry quantities and slippage inputs. Cached candle fixtures retain identical
complete theoretical curves for TWAP5m, RapidX VWAP3m/TWAP3m/instant taker,
including dormant holdings and target removals. No exchange requests are needed
for these comparisons. Real host financial comparison remains a deployment check.

Read-only inspection on both live hosts found:

| Host | Manager DB | Estimated signals | Archive SST bytes | Eligible SST bytes (approximate) |
| --- | --- | ---: | ---: | ---: |
| el01 | `/home/el01/crypto_cta_manager/db` | 6,789,114 | 22,613,273,890 | 12,873,261,446 |
| jp-meta | `/home/ubuntu/crypto_cta_manager/db` | 8,231,215 | 28,861,819,232 | 12,048,391,111 |

Eligibility ended at 2026-10-14 23:50 UTC (2026-10-15 07:50 Shanghai).
Total eligible compressed SST estimate: 24,921,652,557 bytes (23.21 GiB).
Initial records date to August 19. These are RocksDB estimates, not measured
reclaimed bytes or the size of gzip backups.

The archive client was uploaded and inspected without restarting services.
Client SHA-256: `75dfac9f8b28d162a0a49323ddf959e7432449f8992ace8f65e25ba9242ab18f`.
Manager PIDs remained el01 `2471569` and jp-meta `438574`.
Powerleader's private NVMe directory, installed tools, host config and user
service/timer are prepared. Both host switches remain false and the timer is
disabled/inactive. No live seal, export, prune or compaction has been performed.
The `cta_web` archive-owner code is built locally and awaits selected host scope
before deployment. Do not describe this staging result as completed offload.
