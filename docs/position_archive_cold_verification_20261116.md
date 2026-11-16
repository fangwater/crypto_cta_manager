# Cold archive and public file pull verification, 2026-11-16

This records the earlier `d8b1491` candidate. The subsequent minute-batched
Manager and operator-controlled pull changes are documented in
[minute maintenance verification](position_archive_minute_maintenance_20261116.md).

The Rust implementation follows the operator's final instruction: export and
verify a file locally on the source, then SCP the completed file over public IP.
It does not stream a live RocksDB export over the transfer connection. Packages
are grouped by Shanghai date; large days contain bounded segment files.

## Checks

- `cargo fmt --check`, `cargo check --all-targets`, `git diff --check` passed.
- Full crate tests: 582 library and 18 binary tests passed. The existing 17
  operator/benchmark tests and 19 isolated PostgreSQL tests remain ignored.
- The existing manual Python workflow's 12 tests and deployment shell syntax
  check passed.
- Release builds completed for `cta_web`, `cta_position_archive`,
  `cta_archive_pull` and the read-only `cta_order_store_inspect` diagnostic.

Tests cover both JSON and PB hot input, bounded oldest-first batches, persisted
hourly cadence, cold-success/hot-failure and hot-success/READY-failure recovery,
corrupted pending segments, source leases and partial retirement, and independent
bundle restoration. New JSON hot writes retain the original serialized numeric
input rather than introducing a second numeric parse. Cold archival preserves
exact carried inventory/deltas and all four execution models' priced monthly
curves. A waiting retention writer does not block publication through a nested
read-lock acquisition. Stalled SCP/SSH process groups are killed and reaped.

Daily-file tests verify durable packaging, reuse of the same file on retries,
gzip restore, corrupted-file refusal and receipt-gated package removal. Failed
downloads preserve the source DB and package; ordinary SCP retries a small file
from its beginning. A complete matching local `.part` needs no redownload.

## Public transport rehearsal

Evidence directory on powerleader:

```text
/mnt/nvme-raid0-28t/fanghaizhou/crypto_cta_manager_archives/rust-cold-public-rehearsal-20261116T082519Z/
```

The fixture was synthetic, with its own DB identity: 32 expired records moved
from JSON hot storage to PB cold storage, and one recent record stayed hot.
Source helpers and fixture data were staged only under marked `.cache` test
directories. Helper distribution used the existing operator connection; actual
pull/SCP used the pinned numeric public IP without a private/proxy fallback.

On jp-meta the dedicated Rust binary completed source-local packaging, public
SCP, durable local extraction plus independent saved-gzip restoration, exact
source cold/package removal and lost-deletion-acknowledgement retry. All 68
matched protected process identities and the Manager TOML hash stayed identical.
The source test directory was removed; the independent NVMe proof remains.

el01's direct public SSH authentication and expected Manager identity were
verified. Full public metadata requests still timed out intermittently; its
source helper successfully listed the isolated fixture through the operator
connection. **The complete el01 public SCP workflow is not claimed verified.**
The unsuccessful synthetic test directory was removed, and no production data
was reclaimed. A prior isolated bootstrap directory was also cleaned up.

## Operational state

The dedicated local executable is installed at:

```text
/mnt/nvme-raid0-28t/fanghaizhou/crypto_cta_manager_archives/bin/cta_archive_pull
```

`cold-rust-current.txt` under the archive root points to the release package and
its SHA-256 manifest. Private runtime config, public endpoint environment and
trusted known-hosts pins are under the operator's private configuration directory;
none are committed. The configuration example is
`deploy/position_archive/powerleader-cold.example.toml`.

Neither production Manager has been switched/restarted for this feature. No
automatic pull process/timer, live hot pruning, bulk JSON migration or Exec-store
write has been enabled. Both production hot stores remain JSON. Explicit cold
configuration keeps those hot writes JSON and only converts expired cold data.
Live activation remains subject to the repository's exact host/database/operation
authorization rule. See `docs/position_archive_cold_design.md` for configuration,
throughput limits, recovery and the source-local export/SCP sequence.
