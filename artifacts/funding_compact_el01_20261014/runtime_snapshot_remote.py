import base64
import datetime
import hashlib
import http.client
import json
import math
import os
from pathlib import Path
import re
import subprocess
import time
import tomllib
import urllib.parse
root=Path('/home/el01/crypto_cta_manager')
config=tomllib.loads((root/'config/cta-manager.toml').read_text())
def runtime_snapshot():
    processes = []
    for proc in Path('/proc').iterdir():
        if not proc.name.isdigit():
            continue
        try:
            executable = os.readlink(proc / 'exe')
            if not (executable.startswith('/home/el01/binance_exec_')
                    or executable.startswith('/home/el01/crypto_cta_manager/bin/')
                    or (Path(executable.removesuffix(' (deleted)')).name.startswith('python')
                        and b'/home/el01/binance_exec_' in (proc / 'cmdline').read_bytes())
                    or (Path(executable).name == 'nginx' and executable.startswith('/home/el01/'))):
                continue
            ticks = (proc / 'stat').read_text().rsplit(')', 1)[1].split()[19]
            processes.append({'pid': int(proc.name), 'executable': executable, 'start_ticks': ticks})
        except (OSError, IndexError):
            continue
    return {
        'processes': sorted(processes, key=lambda p: p['pid']),
        'unit_pids': {unit: subprocess.check_output(['systemctl', '--user', 'show', unit,
                      '--property=MainPID', '--value'], text=True).strip()
                      for unit in ['crypto-cta-manager-web.service', 'crypto-cta-nginx.service', 'postgresql-cta.service']},
        'config_sha256': hashlib.sha256((root / 'config/cta-manager.toml').read_bytes()).hexdigest(),
        'runtime_sha256': hashlib.sha256((root / 'bin/cta_web').read_bytes()).hexdigest(),
        'trade_configs': {path: hashlib.sha256(Path(path).read_bytes()).hexdigest()
                          for path in config['funding']['trade_engine_configs']},
    }
