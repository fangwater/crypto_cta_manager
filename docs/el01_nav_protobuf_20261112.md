# el01 NAV Protobuf and gzip — 2026-11-12

Release: nav-protobuf-20261112T062843Z.
Backend SHA-256: 13bc274470c70b3988cbbdbd1a67f65f6c03cc7ffd8aa4507dec8185a3952ed5.

The trade06 7D page previously downloaded 93,187,608 bytes of uncompressed
factual JSON, then another approximately 93 MB response for the theoretical
overlay. Its 244 historical symbols each carried 674 points and 17 fields.
Only 12 symbols had fills in the selected window; 86 others had carried
exposure and 146 had zero curves.

The new browser requests Protobuf with shared exact-microsecond time axes,
packed double columns, constant-column encoding and gzip. Portfolio, symbol
and strategy views request their own curves; interval and strategy summaries
stay available. Exactly empty symbol curves are omitted. Theory is fetched
separately without retransmitting actual NAV or funding. All seven plotted
factual values retain their original double bits.

Measured through the original 172.16.30.42:10041 gateway on the same fixed
trade06 window, startMs=1793856550179 and endMs=1794461350179:

| Response | Actual compressed wire bytes |
| --- | ---: |
| Portfolio factual first load | 60,729 |
| Portfolio later repeat | 60,900 |
| Theoretical overlay | 40,506 |
| All nonempty symbol curves | 1,154,074 |
| Strategy curves and strategy funding | approximately 383,000 |

The default factual payload is about 99.935% smaller. The original large JSON
transfer took 130,357 ms. The first new factual query took 5,696 ms; the
theoretical computation took 11,109 ms. Later sampled strategy requests took
202–7,418 ms, with higher costs when dashboard generations refreshed. The
optimization removes the large transfers; reconstruction and overlay computation
still take time.

Actual report and strategy summaries and all 98 nonempty symbol curves matched
the saved pre-deployment fixed-window result. Across actual and theoretical
curves, 616,224 numeric values/timestamps matched exactly. A second comparison
between live JSON and Protobuf funding projections verified another 43,136
funding values, including the current partial coverage. No prices, quantities,
FIFO, attribution or fees were changed.

The complete pre-restart public funding estimate was 303.1167641436829. That
in-memory rate cache is cold after restart and remained partially loaded at
verification. The page retains the explicit loading/partial-coverage reason;
complete old funding coverage was not claimed as revalidated. Public funding
fetch code, rate budget, pacing, egress and collection switches are unchanged.

Production was built against the prior creator-token release's source manifest,
preserving its daily-fifo/float_roundtrip feature. Of its existing source/build
inputs, only src/web.rs, src/web/equity_history.rs, Cargo.toml and Cargo.lock
changed, alongside the new wire schema/module/build script. Unrelated repository
Redis/session changes were excluded. Temporary build inputs were merged in the
existing checkout, then the workspace was restored byte-for-byte; no new checkout
or worktree was created.

The prior frontend was preserved with three bounded NAV substitutions and one
import of the tested Protobuf client. Its original CSS, page structure, account
navigation and creator-token controls remain. The regular repository frontend
also builds with the new direct Protobuf API client.

Validation:

- Production format, check, tests and release build passed: 530 tests passed,
  27 retained their existing isolated-environment skip conditions.
- The frontend production build and six Protobuf/client tests passed.
- A mocked Chromium check verified 7D and view-specific requests with the exact
  production package before deployment.
- Live Chromium through the original gateway verified 7D portfolio, symbol and
  strategy charts, two canvases, gzip/Protobuf responses and no JavaScript errors.
  All browser verification was GET/HEAD only.
- Nginx now sends JS and CSS as gzip: the main 1,166,787-byte entry transferred
  as 413,930 bytes; CSS 98,239 bytes transferred as 22,103 bytes.
- 89 protected non-Nginx processes, the Nginx master, 23 other configuration
  hashes and 11 catalog/snapshot groups were preserved. Nginx workers reloaded
  gracefully for compression. Only Manager restarted; trading, Viz, Config,
  Redis, PostgreSQL and cta_monitor were not restarted.
- All 14 startup account-mode checks passed. Manager PID 3542661 retains CPUs
  0–7 and NRestarts=0. Health is ok, persistence queues are empty and there is
  no persistence error. The target-history index is ready.
- The cold startup loaded local order history before opening the API at
  2026-11-12T06:37:34.520985Z. During startup the API returned 502. By the later
  observation, 1,210 natural target requests had completed, all HTTP 200, with
  processing median/max 1/64 ms.

Rollback backup:
 /home/el01/crypto_cta_manager/backups/nav-protobuf-20261112T062843Z/

Local evidence and packaging scripts:
 /home/fanghaizhou/artifacts/el01_trade06_7d_20261112/

Remote audit:
 /home/el01/crypto_cta_manager/run/nav-protobuf-20261112T062843Z/

Diagnostic sessions were deleted, and browser contexts/profiles closed. No
synthetic target, exchange order or historical signal was submitted.
