# 2026-11-17 native Manager deployment

Public entry: <http://124.223.189.200/manager/>. Local Manager and incremental
collector run under PM2 as `cn-futures-manager` / `cn-futures-sync`; the existing
`cn-futures-sync-tunnel` carries cloud export and gateway traffic. Source identity
remains `sh_dev_chuangyuan01_ctp`. Crypto runtime names and entry points remain
unchanged; the native configuration uses `mode = "cn_futures"`.

Manager release `20261117T145028Z-56ad000` includes native gross-position FIFO NAV,
completed-minute marks, historical account-rate estimates, unified CN fee labels,
source-scoped permissions and frozen native strategy-group netting plans.
The independent Manager PostgreSQL database was backed up before explicitly
adding native NAV/netting tables. Normal startup performs no schema DDL.
The existing `lqg` configure grant was preserved and verified through the gateway.

Starting the external CTP observer generated new instrument/account evidence and
exposed an exporter/collector page-limit mismatch: export accepted 10,000 records
but collection still rejected more than 1,000. Both now share the 10,000 maximum.
A regression covers a full maximum-sized page, a rejected oversized page, process
reopen, frozen-cut resume, atomic commit and equality with original source bytes.
Live collection resumed from the existing durable checkpoint, committed cursor
`5,344,008` and cleared its error. No resnapshot or identity/cursor reset was used.

The native NAV page uses an immutable, evidence-bound gross-position anchor.
The live source now reports trading day `20261118`, with zero positions, orders
and fills. The known empty-history curve is zero; this is not a nonzero live-fill
PnL acceptance. Restart/heartbeat events cannot extend financial coverage beyond
a complete account observation or a validated live trade. Missing coverage is
explicitly unavailable. Actual CNY fees and causal fee estimates stay separate;
CN calculations/UI do not classify fees by maker/taker.

Exec commit `88c3866c` implements optional native limit/FAK ctaker, explicit target
batches, risk/quote/lease gates, unresolved-order blocking and pre-submit child
identity audit. The exact external CTP binary has SHA-256
`e8688ff0e2ce835af44867f1eee968e2904221bd16a2b3428435a32cd3ecbec3`.
It was built in the shared checkout, preserving concurrent native read-only
recovery changes; that artifact is not represented by the commit alone. Build
source hashes and exact artifacts are retained in the ignored deployment receipts.
The existing cloud environment launcher backed up its binary/configuration,
validated the candidate on `sh_dev`, then started the new binary read-only.
Its TOML has no ctaker section, execution is disabled and settlement confirmation
remains false. CTP SDK execution did not run on development/intranet hosts.

The account observer recovered successfully. MD is blocked because the authoritative
dominant service still selects the effective `20261117` contracts while the broker
MD session reports `20261118`. Preserve this date gate. `scheduled_next` is explicitly
a next-session choice; selecting it requires authoritative effective-date/calendar
coverage, rather than removing the check or guessing the next business day.
Account observation continues independently of that MD failure.

Verification: 709 full Rust tests passed before the final targeted fixes; the
latest 78 native tests passed, as did isolated PostgreSQL NAV/permissions/schema
checks (22 tests, plus final focused NAV/netting reruns), TypeScript/native Vite
build, all-target Rust check and release builds. All 166 Exec native library tests
ran on external `sh_dev` (one existing helper ignored). Both official CTP/Yida
feature sets compile locally. Public login, anonymous 401, foreign-source 403,
NAV/static assets, source integrity, coverage rejection and PM2 isolation passed.

Remaining work: authenticated remote execution delivery independent of sync,
exact prepared-order/batch evidence joins for automated fill allocation, live
order/PnL acceptance, complete holiday/weekend mark coverage, ranges beyond
31 days and historical strategy NAV. Cost-reference packs still require explicit
signal/order linkage; missing historical strategy identity is not inferred.
MD-date alignment must be resolved before ctaker can be enabled.
