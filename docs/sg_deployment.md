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

Credentials stay on SG under `/home/ubuntu/.config/crypto-cta-manager/`:
`database.env` and `bootstrap-admin.json`, both mode 0600. Never copy values into
Git, logs or chat. The administrator was created via loopback before exposing
the gateway. Login and authenticated health checks passed, as did static assets
and unauthenticated API rejection.

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
