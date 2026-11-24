#!/usr/bin/env bash
# PM2 launcher: credentials are loaded by the child, never embedded in its config.
set -euo pipefail
umask 077

SOURCE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT="${CN_FUTURES_DEPLOY_ROOT:-$SOURCE_ROOT}"
ENV_FILE="${CN_FUTURES_HOST_ENV:-$HOME/.config/cn-futures-cta-manager/host.env}"
RELEASE="$ROOT/run/current"
if [[ "${1:-}" == pull && -L "$ROOT/run/sync-current" ]]; then
    RELEASE="$ROOT/run/sync-current"
fi
RELEASE="${CN_FUTURES_RELEASE:-$RELEASE}"
[[ -f "$ENV_FILE" ]] || { echo "Missing native Manager host environment file" >&2; exit 1; }
set -a
source "$ENV_FILE"
set +a
ulimit -n 65536
export TOKIO_WORKER_THREADS="${TOKIO_WORKER_THREADS:-2}"
export RAYON_NUM_THREADS="${RAYON_NUM_THREADS:-2}"
export RUST_LOG="${RUST_LOG:-crypto_cta_manager=info}"

case "${1:-}" in
    manager)
        exec "$RELEASE/bin/cn_futures_web" \
            --config "${CN_FUTURES_CONFIG:-$ROOT/config/sync-server-collect.toml}" \
            --bind "${CN_FUTURES_BIND:-127.0.0.1:18211}" \
            --frontend-root "$RELEASE/frontend"
        ;;
    pull)
        exec "$RELEASE/bin/sync-server" \
            --config "${CN_FUTURES_CONFIG:-$ROOT/config/sync-server-collect.toml}" pull
        ;;
    serve)
        exec "$RELEASE/bin/sync-server" \
            --config "${CN_FUTURES_CONFIG:-$ROOT/config/sync-server-export.toml}" serve
        ;;
    *) echo "Usage: run_native_service.sh manager|pull|serve" >&2; exit 2 ;;
esac
