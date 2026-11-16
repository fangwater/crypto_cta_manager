# Address live JSON signal accumulation before format migration

The immediate operator objective is to stop old signal records accumulating on
the live hosts. Full JSON-to-Protobuf replacement and shadow-database migration
are separate future work. This operation retains the deployed JSON format and
uses the running Manager to archive and reclaim an old prefix. It requires one
Manager-only restart to enable the maintenance owner; bulk transfer,
verification, deletion and compaction then run while Manager serves new targets.

## Read-only scope and limits

At the November 16 inspection, both stores still had floor/sealed `(1, 0)` and
no archive receipt. The example boundary below is **exclusive October 15, 2026,
00:00 Asia/Shanghai**, retaining more than the required recent 30 days:

| Host | Manager database | Approximate eligible SST bytes |
| --- | --- | ---: |
| el01 | `/home/el01/crypto_cta_manager/db` | 12,734,910,348 (11.86 GiB) |
| jp-meta | `/home/ubuntu/crypto_cta_manager/db` | 11,895,846,119 (11.08 GiB) |

These are range-size estimates, not promised physical bytes recovered. No live
record has been sealed or removed. The installed live Managers do not contain
the archive-control owner; installing a CLI alone cannot enable reclamation.
The current PB owner also refuses a legacy store, so the standard deployment
script must not be used for this JSON-only maintenance release.

## Prepared JSON-only candidate

The private powerleader NVMe package is:

`/mnt/nvme-raid0-28t/fanghaizhou/crypto_cta_manager_archives/json-retention-20261116T034102Z/`

It contains the two matching executables, operator script/config, exact source
patch and hashes, build/test logs, rehearsal evidence and an unapproved rollout
plan with explicit host/database/column-family/date scope. Only `cta_web` and
`cta_position_archive` are in the executable package; no monitor or Exec binary
is included.

Build provenance is HEAD `974e2ed67fe94842c95b92987ae76c1ef3864e05` with the
code/build/script changes of storage commit `3a60a5e` excluded. That retains the
reviewed JSON maintenance implementation from `9eabf90` and later independent
monitor fixes. The exact reverse patch SHA-256 is
`25b140d64f4c2f2ffa968faec69f9d67bb8dd80cef5c82fe8339b64a579c8620`.
Preparation used the existing checkout and NVMe Cargo target. Every affected
source file and the pre-existing PB release artifacts were hash-verified after
restoration; the normal checkout still contains the PB feature. No alternate
checkout, runtime mixed-format reader or startup format conversion was added.

| Binary | SHA-256 |
| --- | --- |
| `cta_web` | `dd3ded23cfeb406a200b15c8d8d5c93d017458cc210f22cf35624944114467b6` |
| `cta_position_archive` | `24dc4b7efc4b1be51aa9ef4c40c87029000665c23f3f3dd07b018022f145dd57` |

Fresh formatting, complete Rust tests (557 library and 18 binary passes, 17
existing library/operator checks and 19 PostgreSQL checks ignored), 12 Python
tests and the release build passed. A new isolated run used the original 1,520
real JSON sample records: 720 old records were backed up, restored independently
and reclaimed. All 544 retained-month raw signal bytes, opening/closing
inventory hashes, 25 bindings and 1,436 cumulative source/symbol entries matched
after cold restart. Missing-backup deletion and a second writable opener were
rejected; backup alone retained the floor; reclamation retry was idempotent.
The temporary test owner exited and its socket was removed. This sample proof
does not establish full-production priced NAV equivalence or zero I/O impact.

## Host-specific execution after authorization

1. Recheck the selected live host, DB identity, protected processes, current
   targets, configuration, CPU affinity, storage capacity and candidate hashes.
   Preserve its original Manager/client binaries, service logs and configuration
   on NVMe before stopping anything.
2. Install the two matching JSON executables atomically, add only the private
   `[archive].socket` setting to the existing host TOML, and restart only the
   Manager user service. Preserve its service resources and actual CPU affinity;
   do not replace/restart the monitor or trading processes. Verify target
   admission and the owner status before maintenance.
3. Use the package's `pull_position_archive.py` and `powerleader.toml`. The
   config pins the JSON verifier and keeps these backups under the separate
   NVMe `position-json/` directory. Run a read-only preview for the explicitly
   selected host and date, then `--backup`. Transfer is streamed without a large
   duplicate export on the trading host. Independently restore/read back the
   entire compressed original JSON prefix, and durably save its receipt.
4. Run `--reclaim` for exactly that host/date. The script revalidates the local
   backup; Manager independently verifies the sealed prefix, then atomically
   persists its boundary checkpoint and deletes only that `position_updates`
   prefix. Compact only the verified reclaimed range. Record the actual disk
   result and verify retained queries/current publications and protected state.

All long maintenance steps consume shared CPU/storage. The existing owner
lowers CPU priority but has no hard I/O throttle; begin with a manageable range
and monitor publication latency/backlog. No service stop is required for those
steps. A backup/verification/transfer failure prevents deletion. A lost commit
reply or interrupted compaction can be retried with the same saved receipt.

Before pruning, the original executable/config can be restored. After pruning,
retain the checkpoint-aware JSON owner. Returning to the old checkpoint-unaware
owner requires a separately verified restoration of the reclaimed prefix; the
old executable alone is insufficient for correct theoretical reconstruction.

## Keep growth bounded

One reclamation releases accumulated history; it does not cap subsequent growth.
Repeat the explicit-date procedure, retaining at least 30 days plus the execution
buffer. Weekly manual runs retain roughly 30–37 days plus the buffer. Older
signals remain on powerleader NVMe. When powerleader is unavailable, retain
original records and retry later. No continuously connected peer is required.
No timer or automatic date selection has been installed; this preserves the
operator's manual-run instruction. Any later daily automation should call the
same verified workflow under an explicitly chosen host/retention policy.

The independent PB migration candidate remains available for a later format
rollout. It is not a prerequisite for this accumulation cleanup.
