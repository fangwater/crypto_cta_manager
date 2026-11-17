# Project-local RocksDB secondary recovery fix

Base: crates.io `librocksdb-sys` `0.19.0+11.8.1`.
Original registry checksum:
`4f45e86edad8e88efe97dbf384b4e48e1ff0f111eabf154c7b09d7a1e5fb573c`.
Original package sources and upstream license files are retained.

Only `rocksdb/db/version_edit_handler.cc` is modified from that package.
`ManifestTailer::ApplyVersionEdit` removes a dropped CF from `cfds_changed_`
before invoking the base handler. During initial recovery that handler destroys
the CF and returns nullptr; removing that returned nullptr is too late, leaving a
dangling pointer in the set later consumed by `TryCatchUpWithPrimary`.
Native reproduction crashes at `VersionStorageInfo::LevelSummary` on the first
catch-up after opening a manifest containing a drop/recreation history.

Regression: `rocks_source::secondary::tests::secondary_native_catch_up_after_historical_cf_recreation`,
plus the pooled same-name recreation and parallel reader tests. This is an
in-memory pointer lifetime fix; no storage codec, WAL layout, compaction policy
or on-disk version is changed. No dependency sources outside this project are
patched. The project uses Cargo's single `[patch.crates-io]` replacement.
