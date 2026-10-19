# SG Manager deployment

Last verified: 2026-10-16 10:29 UTC.

Independent installation at `/home/ubuntu/crypto_cta_manager`, reachable through
`http://47.131.162.78:4191/manager/`. Backend listens only on `127.0.0.1:18201`.
PostgreSQL 16 is local on `127.0.0.1:5432`; Redis uses `127.0.0.1:6379/0`.
Manager and PostgreSQL use housekeeping CPUs 0–7. The existing trading/IRQ
cores 8–15 and all trading processes are unaffected.

Runtime binaries and frontend were built locally from `d2248e7`, uploaded to
explicit temporary names, SHA-256 verified and atomically installed as release
`20261016T102806Z`. The release
manifest is `webroot/RELEASE-MANIFEST.txt`. Service:
`systemctl --user status crypto-cta-manager-web.service` (enabled).

Database credentials stay on SG in
`/home/ubuntu/.config/crypto-cta-manager/database.env` (mode 0600). Never copy
credential values or password hashes into Git, logs or chat. The initial
administrator was created via loopback before exposing the gateway.

On 2026-10-16, at the operator's request, SG's initial `admin` was replaced with
jp-meta's active administrator `prc`, using the same stored password hash and
retaining SG's local user ID. The password therefore matches jp-meta. Only the
administrator authentication record was copied; jp-meta was read-only. SG's old
sessions were revoked, and the obsolete `bootstrap-admin.json` was removed from
the active credential directory. A restricted authentication backup remains in
`~/.config/crypto-cta-manager/auth-backups/20261016T095004Z/` (directory 0700,
files 0600). Password-hash equality was verified without printing it.
Authenticated administrator/health checks through Nginx passed using a temporary
SG session, which was then removed; anonymous health requests return 401.
The Manager process was not restarted, and no trading configuration changed.

`bybit_exec_trade02` is now provisioned for lgq with the alias `bybit-02`,
an independent namespace and a new empty Exec persistence store. Its six Rust
binaries were built locally from mkt_signal `a73d427d`, uploaded, SHA-256 checked
and atomically installed. `bitget_exec_trade01` remains reserved and disabled.
Kline, funding-rate downloads and the standalone alert monitor remain disabled.
Public requests use the data ENI; account requests bind `172.31.7.124` explicitly.

Bybit's account monitor, persist_manager, Viz and Exec Config run on CPUs 0–7.
`exec-pre-trade`, `trade_engine` and `trade_signal` are **stopped**. Do not run the
full start wrapper as an observation check: pre-trade startup cancels orders.
The existing intra environments remain stopped and their config/positions/history
were not migrated into Exec. The new env securely copies only the existing
arb02 Bybit credentials and trading configuration; all private sockets were
verified on `172.31.7.124`. No account mode, leverage or position was changed.

The Manager lgq login uses the same password hash as JP, with ordinary-user
configure access only to this source. It owns SG's `virtual01 / lgq_cta`, which
has the same ten bindings and multiplier 1 as JP RX02's `virtual02`. The sole
active binding is `lgq_cta_symbol_mean` at 1 share; the other nine stay at 0.
The `lgq_cta_ctaker` order template and all five Exec risk fields were compared
with JP's live `binance_exec_trade05`: ctaker, 100 USDT per order, one order per
batch, max_batch 1, rate limits 400/minute and 200/10 seconds. Initial targets
are numeric zero, and the live RX02 publisher is not connected to SG.

Manager now preserves RX02's explicit `qty: "clear"` contract through parsing,
catalog storage, immutable target archives, shares scaling, Redis publication,
Exec Config readback and the target editor. Numeric zero retains its separate
meaning. No database schema change was needed. The private future publish token is held only in
`~/.config/crypto-cta-manager/bybit02-publish.env` (0600).

An authenticated publication through SG's Nginx gateway changed the prepared
`lgq_cta_symbol_mean` BTCUSDT target to `clear`, confirmed that the catalog and
archive retained signal -1 while the disabled signal-execution setting emitted
signal 0 to Exec, and read `clear` back through Exec Config. An invalid `close`
command returned HTTP 422 without changing the target. All original numeric-zero
targets were then restored through Manager. These two accepted publications
remain as factual audit messages; no execution process was started or exchange
order submitted. The live RX02 publisher remains unconnected to SG.

Local validation passed 349 Rust tests, the independent Redis integration test
(including native Bybit and Bitget clear publication/readback), seven Python
publisher tests, 18 frontend tests and the production frontend build. All four
installed Manager binary hashes and the running executable match the release
manifest. Ten protected process identities, trading configuration hashes,
binding/follow settings and risk/order parameters were unchanged. The temporary
verification session was removed.

The authenticated `/exec_trade02/`, `/exec_trade02/config/` and snapshot routes
return 200; its WebSocket upgrade returns 101. Anonymous requests return 401.
Manager's Bybit unified-account feed is healthy, all 47 RX02 symbols are active
in the refreshed Bybit rule cache, and the configuration delivery queue is empty.
Viz/Config listen only on `127.0.0.1:10042` / `127.0.0.1:18162`.
Deployment backups are under
`~/.config/crypto-cta-manager/deploy-backups/20261016T100107Z-bybit02/`; parameter
and process verification is recorded in mkt_signal's
`artifacts/sg_bybit02_exec_deploy/20261016T100107Z/validation.json`.

For updates, build locally then run:

```bash
scripts/deploy_host.sh --target sg --manager-only --skip-build
scripts/install_jp_meta_nginx.sh --target sg
```

The publisher leaves live config intact and uploads a `.template`. The gateway
mapping uses one `external:` snippet, preserving SPA routes when the site's
configuration is regenerated. Initial gateway backups are in the runtime root.
First installation requires PostgreSQL packages, the host-guarded
`scripts/install_postgresql_cta_sg.sh`, and an explicit `cta_web --init-db` with
the database environment loaded before starting the service. Never initialize
an existing database or overwrite the credential files.
