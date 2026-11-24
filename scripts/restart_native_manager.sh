#!/usr/bin/env bash
# Environment-local restart of the domestic Manager only; preserve runtime logs.
set -euo pipefail
SOURCE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT="${CN_FUTURES_DEPLOY_ROOT:-$SOURCE_ROOT}"
if [[ "${1:-}" != --execute ]]; then
    [[ $# == 0 ]] || { echo 'Usage: restart_native_manager.sh [--execute]' >&2; exit 2; }
    echo "[DRY-RUN] env=$ROOT action=restart process=cn-futures-manager"
    exit 0
fi
[[ $# == 1 ]] || { echo 'Usage: restart_native_manager.sh [--execute]' >&2; exit 2; }
python3 - "$ROOT" <<'PY'
from pathlib import Path
from datetime import datetime, timezone
import json, shutil, subprocess, sys
root = Path(sys.argv[1]).resolve()
raw = subprocess.check_output(['pm2', 'jlist'], text=True)
rows = [row for row in json.loads(raw[raw.index('['):]) if row['name'] == 'cn-futures-manager']
for row in rows:
    if Path(row['pm2_env']['pm_cwd']).resolve() != root:
        raise SystemExit('Manager process belongs to another environment')
archive = root / 'run/ops-logs' / datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ')
archive.mkdir(parents=True, mode=0o700)
for row in rows:
    for key in ('pm_out_log_path', 'pm_err_log_path'):
        path = Path(row['pm2_env'][key])
        if path.is_file():
            shutil.copy2(path, archive / path.name)
print('Preserved Manager logs:', archive)
PY
CN_FUTURES_DEPLOY_ROOT="$ROOT" pm2 startOrRestart "$SOURCE_ROOT/deploy/cn_futures/ecosystem.config.cjs" --only cn-futures-manager
pm2 save --force >/dev/null
