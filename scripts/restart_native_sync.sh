#!/usr/bin/env bash
set -euo pipefail
umask 077
source_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
root="${CN_FUTURES_DEPLOY_ROOT:-$source_root}"
case "${1:-}" in
  pull) name=cn-futures-sync ;;
  serve) name=cn-futures-exporter ;;
  *) printf 'Usage: %s pull|serve [--execute]\n' "$0" >&2; exit 2 ;;
esac
if [[ "${2:-}" != --execute ]]; then
  printf 'env=%s process=%s action=restart dry_run=true\n' "$root" "$name"
  exit 0
fi
python3 - "$root" "$name" <<'PY'
import json, shutil, subprocess, sys, tempfile
from pathlib import Path
root, name = Path(sys.argv[1]).resolve(), sys.argv[2]
raw = subprocess.check_output(["pm2", "jlist"], text=True)
rows = [p for p in json.loads(raw[raw.index("["):]) if p["name"] == name]
for row in rows:
    env = row["pm2_env"]
    if Path(env["pm_cwd"]).resolve() != root:
        raise SystemExit("native sync process belongs to another environment")
    backups = root / "run/ops-logs"
    backups.mkdir(parents=True, exist_ok=True)
    archive = Path(tempfile.mkdtemp(prefix=f"{name}-", dir=backups))
    for key in ("pm_out_log_path", "pm_err_log_path"):
        log = Path(env[key])
        if log.is_file():
            shutil.copy2(log, archive / log.name)
PY
CN_FUTURES_DEPLOY_ROOT="$root" pm2 startOrRestart "$source_root/deploy/cn_futures/ecosystem.config.cjs" --only "$name"
pm2 save --force >/dev/null
