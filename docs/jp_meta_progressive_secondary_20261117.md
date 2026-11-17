# jp-meta progressive signal archive and reusable readers — 2026-11-17

Release: `20261117T041012Z-progressive-secondary`. Only the jp-meta host-level
Manager was replaced in this release. el01 was subsequently deployed separately;
see [its deployment evidence](el01_progressive_secondary_20261117.md).
The frontend, service unit/drop-ins,
monitor, Exec, Viz, Config, Redis, Nginx, PostgreSQL, account funding switches,
and trading bindings were preserved.

The candidate combines the committed progressive archive implementation at
`50b5ad408c4f449719214c13905260e5885f2c76` with the tested, uncommitted reusable
source-reader changes. It must not be identified as a clean build of that commit.
The source and binary hashes are recorded in
`source-secondary-verification-20261117.json` on powerleader's NVMe archive.
See [reader design and validation](source_secondary_readers_20261117.md).

## Installation and interruption

The running `cta_web` SHA-256 is
`b05991075d4610c3eb1f31a8141af58fcf12bb93dee25407fec5c91455ce0dcb`.
Seven Manager/helper binaries were replaced through verified temporary files
and atomic renames. The running Manager remains pinned to CPU 24.

The old Manager stop began at 04:27:36.435 UTC. Its existing 15-second systemd
stop timeout expired, so systemd killed the old process. The closed checkpoint
then took 1.4535 seconds. The replacement started at 04:27:54.54 UTC and bound
the API at 04:28:08.161 UTC: approximately 32 seconds of Manager API interruption.
Trading processes continued running. Factual NAV was subsequently unavailable
while its first background generation rebuilt; target publication resumed before
that rebuild completed.

Both old persistence queues were zero immediately before stopping. The final
old accepted target was `rbf_big`, with `updated_at_us=1794889655969717`.
The replacement's durable conversion marker records exactly this original
archive head, providing evidence that this final accepted target was preserved.

## Archive policy

The Manager DB identity is `71c586be-58f6-4cab-966b-e61b7902f5ec`.
Cold data lives under `/home/ubuntu/crypto_cta_manager/cold-signals`.

- New signals use PB immediately. Existing JSON converts progressively at its
  original keys; records outside 33 days plus the execution guard move to PB cold.
- The worker checks once per minute, requires 30 seconds of quiet, CPU at most
  35%, I/O pressure at most 1%, no publication/archive backlog, and 10 GiB reserve.
- Each cold attempt admits at most 1,000 records / 64 MiB. Retained hot
  conversion admits at most 1,000 records / 8 MiB with a 50 ms batch-building
  budget. These bounds do not constitute a hard disk-operation latency bound.
- Explicit automatic range compaction is disabled. The independent persisted
  compaction cooldown is 3,600 seconds. Startup does not force migration, flush
  or range compaction to demonstrate progress.
- Manager initiates no network transfer. Powerleader's Rust `cta_archive_pull`
  is installed on NVMe with a private, host-local jp-meta configuration and
  pinned public IPv4 SSH. Only inventory preview was invoked during deployment;
  no pull daemon, timer, source cold export or source cold deletion was enabled.

## Recoverable original checkpoint

The complete original Manager DB was captured while its owner was stopped and
the RocksDB POSIX lock was held. Immutable SST files were hardlinked; WAL,
CURRENT, MANIFEST and OPTIONS files are independent copies. In particular,
never hardlink WALs: native recovery can truncate them.

Precopy preserved 159 stable WALs (6,508,046,177 bytes). The stop window linked
29,550 immutable files and copied another 51,528,506 bytes. All 29,717 captured
files passed the file catalog/size and mutable-file hash checks. An independent
native read opened the checkpoint and decoded its first and last JSON records.

The source produced a complete closed package before any public transfer:
`manager-db-20261117T041012Z-progressive-secondary.tar.zst`, 16,499,481,112 bytes,
SHA-256 `4cf2fadddce8cc4aa6d93961ca93ffacd5c39321e2d3252109447ae65561750e`.
It contains only the Manager DB and checkpoint inventory, with no host
credentials or runtime TOML. Packaging ran at low CPU/I/O priority on CPU 36.

Deployment evidence is retained under
`/home/fanghaizhou/artifacts/jp_meta_secondary_rollout_20261117`.
The attempted NVMe checkpoint destination was
`/mnt/nvme-raid0-28t/fanghaizhou/crypto_cta_manager_archives/backups/jp-meta/20261117T041012Z-progressive-secondary`.
The operator clarified that the operational backup flow is the verified,
operator-controlled cold-segment pull and source reclamation, rather than a full
database return. The extra whole-checkpoint transfer and its waiting restore job
were stopped. The 16 incomplete local chunks (9,685,899,800 bytes) were removed
after confirming the complete original snapshot/package still existed remotely.
No completed full NVMe backup or local full-restore verification is claimed.
The remote one-time predeployment checkpoint and compatible fallback are
retained; they are separate from routine cold-segment backup. No full-database
transfer should be scheduled as part of this policy.

## Verification and rollback

The post-switch checks preserved 132 protected process identities, 57
configuration files and 11 immutable snapshot/configuration tables, except for
the explicitly enabled Manager archive settings. Publication queues stayed empty
and natural target requests returned HTTP 200. Startup HTTP 503 responses were
NAV-loading requests. The theoretical index reached readiness after loading
10,203,225 messages. Factual NAV's first generation completed at 04:56:44.485 UTC
after 1,716.322 seconds, covering eight enabled sources. This approximately
29-minute analytical warmup is separate from the 32-second API interruption.

Two fixed historical `1000PEPEUSDT` account queries (trade01 and trade03, six
hours ending before deployment) returned exactly identical financial reports,
with 222 and 123 fills respectively. Their SHA-256 hashes are
`8d6a6f0de1cbde1bbbca5daa2b00ca751662e389f9cbd9d4049985d8ef57751a`
and `cfd2e893fa213d41107c0739cceb79d7dac63becd9b6db7cd2970d22786c5ea1`.
Temporary diagnostic sessions were deleted after verification.

After the post-warm reader population stabilized, four dashboard refreshes
took 4,600, 4,589, 4,864 and 4,810 ms. No new source secondary opened during the
180-second observation. Six successive 30-second process CPU measurements were
17.23%, 2.10%, 18.27%, 2.43%, 17.87% and 2.60% of one core, averaging 10.08%.
This covers refresh and idle phases on the unchanged CPU 24; it is a short
production observation, not a guarantee for all future query loads. The previous
repeated-open refreshes took approximately 40–44 seconds.

The first idle-gated cold attempt ran at 04:58:38 UTC. By the 05:01 observation,
three attempts had moved 2,003 expired records, with the later batches capped at
1,000. The first attempt reclaimed one fully expired 1,279-byte SST in 34 ms;
later attempts deferred unsuitable ranges without manual compaction. All cold
errors were null and publication queues remained empty. The first cold segment
was also visible to the public Rust pull's read-only preview; preview did not
back it up or delete it. Retained hot JSON conversion still reports zero records
at this point because the worker prioritizes the older expired prefix.

Final verification after stopping the extra full transfer again preserved all
132 protected processes, 57 configuration files and 11 key tables. Both fixed
financial hashes still matched; health was `ok`, both publication persistence
queues were zero, 11,499 expired records had migrated, and the cold error was
null. No routine cold segment had been pulled or deleted at that verification.

The operator then explicitly requested a live Rust pull test. One finite
`--run --reclaim-source --max-segments 2` invocation backed up the two oldest
eligible cold segments over pinned public IPv4 SCP to powerleader NVMe, verified
their compressed files and independently restored contents before reclamation,
and removed the corresponding source segments and locally exported source
packages. The segments contained 3 and 1,000 records (1,003 total); their local
compressed files were 6,773 and 53,032 bytes. Local backup receipts and
`source-removed.json` proofs remain under `cold-public/jp-meta` on NVMe.
The source retained its carried-inventory boundary and durable retirement
receipts. After the test, all protected processes/configuration/tables and both
historical financial report hashes again matched; health was `ok` with both
publication persistence queues at zero. Manager PID 902178 did not restart.
No watch process or timer was enabled. Evidence is in `live-cold-pull.json`,
`live-cold-pull-proof.json` and `after-live-pull.jsonl` in the local rollout directory.

Manual public pull remains explicitly controlled by the operator. On powerleader,
inventory preview is:

```bash
CTA_ARCHIVE_JP_META_PUBLIC_IP=13.115.227.29 \
  /mnt/nvme-raid0-28t/fanghaizhou/crypto_cta_manager_archives/bin/cta_archive_pull \
  --config /home/fanghaizhou/.config/crypto-cta-manager/archive-pull/jp-meta.toml \
  --host jp-meta
```

Add `--run --reclaim-source` only when deliberately requesting a verified pull
and source reclamation. The default handles one cold segment; an explicit finite
`--max-segments` batch remains manual. Do not enable `--watch` or a timer without
a new operator instruction.

The old JSON-only binary cannot read the new PB suffix. Do not replace the
current archive with the original checkpoint after accepting new signals.
The release retains a compatible progressive-format fallback `cta_web.fallback`
(SHA-256 `e56ea5b360d5c076086472a3a5c5229e9b82d730136d2ef33d1197cc6fc04047`).
If rollback is necessary, stop only Manager and switch to this fallback while
preserving the current database and archive configuration. This fallback retains
the former repeated read-only source opens; it is not the old JSON-only binary.
