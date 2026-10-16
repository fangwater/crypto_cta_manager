# SG Manager deployment

Last verified: 2026-10-16 UTC.

Independent installation at `/home/ubuntu/crypto_cta_manager`, reachable through
`http://47.131.162.78:4191/manager/`. Backend listens only on `127.0.0.1:18201`.
PostgreSQL 16 is local on `127.0.0.1:5432`; Redis uses `127.0.0.1:6379/0`.
Manager and PostgreSQL use housekeeping CPUs 0–7. The existing trading/IRQ
cores 8–15 and all trading processes are unaffected.

Runtime binaries and frontend were built locally from `399c9b0`, uploaded to
explicit temporary names, SHA-256 verified and atomically installed. The release
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

`deploy/sg/cta-manager.toml` reserves disabled `bybit_exec_trade02` and
`bitget_exec_trade01` sources. Neither Exec exists yet. Enable and configure
sources only when the corresponding independent Exec is ready; do not point
these paths at intra history. Kline, funding-rate downloads and monitor are
disabled. Future public queries use data ENI `172.31.10.217`; private queries
must use the source account's first configured trading IP.

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
