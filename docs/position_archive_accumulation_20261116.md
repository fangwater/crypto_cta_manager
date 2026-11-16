# Address live JSON signal accumulation before format migration

The immediate operator objective is to stop old signal records accumulating on
the live hosts. Full JSON-to-Protobuf replacement and shadow-database migration
are separate future work. This operation retains the deployed JSON format and
uses the running Manager to archive and reclaim an old prefix. It requires one
Manager-only restart to enable the maintenance owner; bulk transfer,
verification, deletion and compaction then run while Manager serves new targets.

## Read-only scope and limits

At the November 16 inspection, both stores still had floor/sealed `(1, 0)` and
no archive receipt. The operator requested a three-day redundancy window on top
of the 30-day analysis window. The owner now enforces **33 days plus the existing
execution guard**. For a November 16 run the proposed exclusive boundary is
**October 14, 2026, 00:00 Asia/Shanghai**. The earlier October 15 proposal and
30-day candidate have been superseded. Fresh read-only previews found:

| Host | Manager database | Approximate eligible SST bytes |
| --- | --- | ---: |
| el01 | `/home/el01/crypto_cta_manager/db` | 12,316,926,711 (11.47 GiB) |
| jp-meta | `/home/ubuntu/crypto_cta_manager/db` | 11,435,552,098 (10.65 GiB) |

These are range-size estimates, not promised physical bytes recovered. No live
record has been sealed or removed. The installed live Managers do not contain
the archive-control owner; installing a CLI alone cannot enable reclamation.
The current PB owner also refuses a legacy store, so the standard deployment
script must not be used for this JSON-only maintenance release.

## Prepared JSON-only candidate

The private powerleader NVMe package is:

`/mnt/nvme-raid0-28t/fanghaizhou/crypto_cta_manager_archives/json-retention-33d-20261116T041437Z/`

It contains the two matching executables, operator script/config, exact source
patch and hashes, build/test logs, rehearsal evidence and an unapproved rollout
plan with explicit host/database/column-family/date scope. Only `cta_web` and
`cta_position_archive` are in the executable package; no monitor or Exec binary
is included.

Build provenance is HEAD `b325cf4a135060d4febec7b8a3703714ef86cf8d` with the
code/build/script changes of storage commit `3a60a5e` excluded. That retains the
reviewed JSON maintenance implementation from `9eabf90` and later independent
monitor fixes and the 33-day guard. The exact reverse patch SHA-256 is
`25b140d64f4c2f2ffa968faec69f9d67bb8dd80cef5c82fe8339b64a579c8620`.
Preparation used the existing checkout and NVMe Cargo target. Every affected
source file and the pre-existing PB release artifacts were hash-verified after
restoration; the normal checkout still contains the PB feature. No alternate
checkout, runtime mixed-format reader or startup format conversion was added.

| Binary | SHA-256 |
| --- | --- |
| `cta_web` | `fb9baa62d00bd001c5c192161572c14449076196804bd52dd4f3f0afd1a16fff` |
| `cta_position_archive` | `c1c096303ba77937393c96e36f093b6e3f4ce3a39ca262db4cefc2f8bcdcd0eb` |

Fresh formatting, complete Rust tests (558 library and 18 binary passes, 17
existing library/operator checks and 19 PostgreSQL checks ignored), 12 Python
tests and the release build passed. A new isolated run used the original 1,520
real JSON sample records: 720 old records were backed up, restored independently
and reclaimed. All 544 retained-month raw signal bytes, opening/closing
inventory hashes, 25 bindings and 1,436 cumulative source/symbol entries matched
after cold restart. Missing-backup deletion and a second writable opener were
rejected; backup alone retained the floor; reclamation retry was idempotent.
The temporary test owner exited and its socket was removed. This sample proof
does not establish full-production priced NAV equivalence or zero I/O impact.
The compiled candidate CLI accepted the October 14 preview and rejected October
15 under its real clock; the matching owner binary contains the same 33-day
guard. A regression test confirms the too-recent export fails before sealing
or writing retention metadata. The preceding 30-day package is marked
superseded and must not be deployed.

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

## Reduce the one-time restart interruption

Finish uploads, candidate checksum checks, executable/log/config preservation
and configuration preparation before stopping Manager. Stop/start only one host
at a time. Before stopping, require a healthy, empty archive backlog; normal
graceful shutdown closes admission and drains accepted archive work (currently
up to ten seconds). Keep the existing service stop resources and do not force
kill a process to shorten the window. Start the replacement immediately; begin
bulk archival only after publication readiness has been verified.

Current startup already runs account-mode verification with concurrency four
and starts NAV/history warming after binding the API. Keep required account-mode
and publication-permission checks. A systemd `active` state alone does not prove
publication readiness; observe the API/catalog and archive health before
declaring the interruption over. No measured upper bound for the live restart
has been established.

Exec, Redis and the monitor remain running with their current targets. Requests
sent during the Manager gap can fail. The generic `manager_publish_client.py`
currently has no automatic request retry, and arbitrary POST retries do not
guarantee ordered, duplicate-free history after an ambiguous response. If every
signal must survive the gap, coordinate an upstream queue/ordered-delivery
barrier first, with safe handling of uncertain acknowledgements. Do not claim
this exists merely because the Manager drains its own already accepted queue.
Otherwise choose a quiet publisher interval and report failed requests rather
than promising an invisible restart.

## Keep growth bounded

One reclamation releases accumulated history; it does not cap subsequent growth.
Repeat the explicit-date procedure, retaining at least 33 days plus the execution
buffer. Weekly manual runs retain roughly 33–40 days plus the buffer. Older
signals remain on powerleader NVMe. When powerleader is unavailable, retain
original records and retry later. No continuously connected peer is required.
No timer or automatic date selection has been installed; this preserves the
operator's manual-run instruction. Any later daily automation should call the
same verified workflow under an explicitly chosen host/retention policy.

The independent PB migration candidate remains available for a later format
rollout. It is not a prerequisite for this accumulation cleanup.
