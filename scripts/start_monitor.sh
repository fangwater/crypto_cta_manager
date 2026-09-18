#!/usr/bin/env bash
# Register and start the CTA monitor under pmdaemon.
# Deployed beside bin/ and config/ in the Manager remote root.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BASE_DIR="$SCRIPT_DIR"
PROC_NAME="${PMDAEMON_NAME:-cta_monitor}"
BIN_PATH="${BASE_DIR}/bin/cta_monitor"
CONFIG_PATH="${BASE_DIR}/config/cta-manager.toml"
ENV_FILE="${CTA_MANAGER_ENV_FILE:-$HOME/.config/crypto-cta-manager/database.env}"
RUST_LOG_VALUE="${RUST_LOG:-crypto_cta_manager=info}"

PMDAEMON_BIN="${PMDAEMON_BIN:-pmdaemon}"
if [[ "$PMDAEMON_BIN" != */* ]] && ! command -v "$PMDAEMON_BIN" >/dev/null 2>&1; then
    for candidate in "$HOME/.local/bin/pmdaemon" /usr/local/bin/pmdaemon; do
        if [[ -x "$candidate" ]]; then
            PMDAEMON_BIN="$candidate"
            break
        fi
    done
fi
if ! command -v "$PMDAEMON_BIN" >/dev/null 2>&1; then
    echo "[ERROR] pmdaemon not found" >&2
    echo "[HINT] install with: cargo install pmdaemon" >&2
    exit 1
fi

if [[ ! -x "$BIN_PATH" ]]; then
    echo "[ERROR] monitor binary not found or not executable: $BIN_PATH" >&2
    exit 1
fi
if [[ ! -f "$CONFIG_PATH" ]]; then
    echo "[ERROR] monitor config not found: $CONFIG_PATH" >&2
    exit 1
fi

# Re-register idempotently so a redeploy picks up script/env changes.
if [[ -x "${SCRIPT_DIR}/stop_monitor.sh" ]]; then
    "${SCRIPT_DIR}/stop_monitor.sh"
fi

quote() { printf '%q' "$1"; }
cmd="set -a; if [[ -f $(quote "$ENV_FILE") ]]; then source $(quote "$ENV_FILE"); fi; set +a; exec $(quote "$BIN_PATH") --config $(quote "$CONFIG_PATH")"

echo "[INFO] starting ${PROC_NAME} under pmdaemon"
"$PMDAEMON_BIN" start /bin/bash \
    --name "$PROC_NAME" \
    --cwd "$BASE_DIR" \
    --env "RUST_LOG=${RUST_LOG_VALUE}" \
    -- -lc "$cmd"

echo "[INFO] ${PROC_NAME} started"
echo "[INFO] Logs: ${PMDAEMON_BIN} logs ${PROC_NAME} --follow"
echo "[INFO] Status: ${PMDAEMON_BIN} list"
