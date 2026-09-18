#!/usr/bin/env bash
# Stop and deregister the CTA monitor pmdaemon process.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BASE_DIR="$SCRIPT_DIR"
PROC_NAME="${PMDAEMON_NAME:-cta_monitor}"

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
    exit 1
fi

if "$PMDAEMON_BIN" info "$PROC_NAME" >/dev/null 2>&1; then
    "$PMDAEMON_BIN" delete "$PROC_NAME"
    echo "[INFO] ${PROC_NAME} deleted"
else
    echo "[INFO] ${PROC_NAME} is not registered"
fi

# Clean up a leaked monitor process that outlived the pmdaemon registration.
mapfile -t leaked_pids < <(pgrep -f "${BASE_DIR}/bin/cta_monitor" || true)
if [[ ${#leaked_pids[@]} -gt 0 ]]; then
    echo "[WARN] leaked monitor process(es): ${leaked_pids[*]}; sending SIGTERM"
    kill "${leaked_pids[@]}" >/dev/null 2>&1 || true
    deadline=$((SECONDS + 10))
    while [[ $SECONDS -lt $deadline ]]; do
        mapfile -t leaked_pids < <(pgrep -f "${BASE_DIR}/bin/cta_monitor" || true)
        [[ ${#leaked_pids[@]} -eq 0 ]] && break
        sleep 1
    done
    if [[ ${#leaked_pids[@]} -gt 0 ]]; then
        echo "[WARN] SIGTERM timeout, sending SIGKILL"
        kill -9 "${leaked_pids[@]}" >/dev/null 2>&1 || true
    fi
fi
