# RapidX instant taker deployment evidence

Final jp-meta release: `20261022T065757ZInstantTakerCache`, runtime `dbc0435`,
frontend `20fdd59`. See `docs/jp_meta_instant_taker_20261022.md` for the feature,
cache fix, verification conditions and limitations.

Final evidence:

- `cachefix_release.json`: runtime and frontend hashes.
- `comparison_summary.json`, `after_capture.jsonl`: eight unchanged factual
  reports, unchanged existing references, complete new instant taker results.
- `open_audit.jsonl`: 110 independent candle-open and taker-fee checks.
- `browser_result.json`: eight desktop/mobile scenarios passed.
- `auth_browser_result.json`: two transient-auth recovery scenarios passed.
- `repeat_verification.jsonl`: identical repeat results, one isolated immediate
  cache hit with zero new requests, health and six Exec gateway checks. The
  broader batch's global counter also includes concurrent background requests.
- `protected_final.jsonl`: protected process/config/table and CPU checks.
- `cold_start.log`, `cachefix_cold_start.log`: cold index completion evidence;
  differing concurrent workloads prevent a direct performance comparison.
- `cleanup.json`, `browser_cleanup.json`, `archive_manifest.json`: exact-path
  cleanup and hashes of the removed private temporary archive.

Other logs preserve intermediate deployment and diagnostic observations; use
the files above for final conclusions. Scripts are audit records specific to
this deployment. Remote bodies are composed with `remote_common.py`; some also
require the auth setup from `capture_body.py`. The only diagnostic database
writes create temporary auth sessions, removed in `finally`; no Exec writes.

After successful verification, the operator requested removal of this round's
temporary archives/backups. Raw before/after responses, screenshots, staging
directories and browser profile are therefore no longer retained. Re-running
the comparison or browser wrappers requires fresh captures/staging and a new
isolated browser session; these scripts are not a standalone deployment tool.
Manager's live target archive and Kline cache were retained.
