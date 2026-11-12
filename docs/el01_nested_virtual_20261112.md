# el01 nested Virtual accounts — 2026-11-12

Release: `nested-virtual-20261112T074422Z`.
Feature commit: `0f2ca2b2bc2aa43f8d1fc49a02477a2fcaece5f8`.
Backend SHA-256: `63178126d589b4551d64dc10570243b5bd4b087c4c64a807b325043f9a71fa4f`.

The Virtual editor's Add selector now accepts position strategies and child
Virtual accounts. Each child has its own literal multiplier, inherits the child's
execution templates, and contributes its expanded shares to the parent. The editor
previews the combined leaf bindings; real accounts apply their own follow multiplier
after expansion. Descendant edits propagate through all ancestors to their actual
followers. Matching publication identities sum, while cycles, conflicting templates,
invalid ratios and overflowing shares reject the whole save. See
[Virtual account semantics](virtual_accounts.md).

Production was built from the running `nav-protobuf-20261112T062843Z` source
manifest, preserving its `daily-fifo` and `serde_json/float_roundtrip` features.
Only `src/web.rs` and `src/virtual_accounts.rs` changed among its existing Rust
inputs, with the new graph module and explicit schema addition. Its existing
Redis/session/publication differences were preserved. Temporary build inputs were
applied in the existing checkout and restored byte-for-byte afterward.

The frontend retains the prior production NAV transport module, CSS and unrelated
pages. Its Virtual page was compiled from the committed source and inserted in
place of the previous component, reusing the existing React/UI runtime. The account
follow explanation was also updated. Existing immutable assets remain available
for cached HTML. The exact frontend file hashes and build inputs are recorded in
the release manifest.

Before installation, the full Manager PostgreSQL database was backed up to a
507,036,777-byte custom-format dump, and `pg_restore --list` verified it. Manager
was then stopped gracefully to pause configuration writes. The reviewed
`scripts/apply_nested_virtual_schema.sql` added only the empty child-reference
table and its reverse index. Existing catalog and snapshot projections were
checked before and after the DDL. Normal startup does not execute schema changes.
The binary and frontend symlink were replaced atomically, then only Manager was
started. Nginx configuration and all other services were retained.

Validation before installation:

- The committed implementation passed its full Rust test suite, 67 frontend
  tests, frontend build, and 19 isolated PostgreSQL integration tests, including
  nesting, diamond paths, propagation, zero stops and transactional rollback.
- The production baseline passed formatting, check, 533 Rust tests and release
  build; 28 tests retained their isolated-environment skip conditions. Immutable
  allocation fingerprints passed the read-only preflight on el01.
- Mocked Chromium tested the exact production frontend: child selection without
  an order-template catalog, self/ancestor exclusion, additive 2.2-share preview,
  the saved `children` request body, zero and invalid ratios, and mobile layout.
  No requests were sent to production during that test.

Live verification completed:

- The original seven Virtual accounts retained their direct bindings, ownership,
  follower lists and permissions. Their new child and parent lists are empty,
  and expanded bindings exactly equal their previous direct bindings.
- All 92 protected process identities, all 24 configuration hashes and 20
  catalog/snapshot projections were unchanged. These projections exclude normal
  live target values and update timestamps. No trading, Viz, Config, Redis,
  PostgreSQL, Nginx or monitor process was restarted.
- Manager PID `484149` retains CPUs 0–7 and `NRestarts=0`. Health is `ok`,
  archive/mirror queues are empty and no persistence or refresh error is present.
  The target-history index is ready after 6,082,084 messages.
- All 14 startup account-mode checks passed. The API opened at
  `2026-11-12T07:55:10.418454Z`, about 339 seconds after the installation began.
  During the verification observation, 224 natural target submissions completed,
  all HTTP 200; no synthetic target was submitted.
- Chromium through the original gateway verified the child selector, a 0.6-ratio
  draft preview and mobile layout with no JavaScript errors or horizontal overflow.
  The diagnostic ordinary user retained its two visible Virtual accounts. All
  browser requests were GET/HEAD; draft edits were not saved to production.
- The fixed trade06 7D portfolio query (startMs `1793856550179`, endMs
  `1794461350179`) returned an exactly identical factual report, including all
  674 points. Protobuf/gzip remains active: the two responses were 60,834 and
  60,780 compressed bytes. The public funding-rate cache is disposable and cold
  after restart; its coverage was not included in this factual-order comparison.
- Diagnostic sessions and browser contexts were closed. Duplicate staged binary
  and frontend archives were removed after verification; the rollback backup and
  small audit files are retained.

Rollback backup:
`/home/el01/crypto_cta_manager/backups/nested-virtual-20261112T074422Z/`.
It contains the full database dump, previous binary and frontend link, schema
script, baseline and manifest. The old backend must not be restored after users
add child references without first reviewing and removing/materializing those
references; it would ignore their contributions.

Local evidence and packaging scripts:
`/home/fanghaizhou/artifacts/el01_nested_virtual_20261112/`.
Remote audit:
`/home/el01/crypto_cta_manager/run/nested-virtual-20261112T074422Z/`.
