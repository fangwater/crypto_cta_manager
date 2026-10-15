"""Verify strategy creation through el01's live Manager gateway.

All verification targets are empty and unbound. Session tokens stay in memory.
No schema, account binding, Exec store, or trading configuration is changed.
"""

from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import secrets
import shutil
import subprocess
import sys
import tomllib
import urllib.error
import urllib.parse
import urllib.request

ROOT = Path('/home/el01/crypto_cta_manager')
RELEASE = '20261015T040000Z'
STAGE = ROOT / 'deploy-staging' / ('position-owner-' + RELEASE)
BACKUP = ROOT / 'backups' / ('position-owner-' + RELEASE)
PG_BIN = Path('/home/el01/.local/opt/postgresql/16/bin')
ORIGIN = 'http://127.0.0.1:10051'
CONFIG = tomllib.loads((ROOT / 'config/cta-manager.toml').read_text())


def manager_pid():
    return int(subprocess.check_output([
        'systemctl', '--user', 'show', 'crypto-cta-manager-web.service',
        '-p', 'MainPID', '--value',
    ], text=True))


def capture_state():
    paths = [ROOT / 'config/cta-manager.toml', ROOT / 'nginx/nginx.conf',
             Path.home() / '.config/systemd/user/crypto-cta-manager-web.service',
             Path.home() / '.config/systemd/user/crypto-cta-nginx.service']
    paths += [Path(p) for p in CONFIG.get('kline', {}).get('trade_engine_configs', [])]
    processes = {}
    for proc in Path('/proc').iterdir():
        if not proc.name.isdigit() or int(proc.name) == os.getpid():
            continue
        try:
            comm = (proc / 'comm').read_text().strip()
            cmd = (proc / 'cmdline').read_bytes().replace(b'\0', b' ').decode(errors='replace')
            if comm == 'cta_web':
                continue
            protected = any(name in comm.lower() for name in [
                'trade_engine', 'persist_manager', 'exec-pre-trade', 'exec_pre_trade',
                'account_monitor', 'trade_signal', 'viz', 'nginx', 'redis-server', 'cta_monitor',
            ]) or (comm.startswith('python') and any(name in cmd for name in [
                'exec_config_server', 'viz_server',
            ])) or (comm == 'postgres' and ' -D ' in cmd)
            if protected:
                stat = (proc / 'stat').read_text().rsplit(')', 1)[1].split()
                processes[proc.name] = {'comm': comm, 'start_ticks': stat[19]}
        except (OSError, IndexError):
            continue
    return {
        'manager_pid': manager_pid(),
        'webroot': str((ROOT / 'webroot').resolve()),
        'protected_processes': processes,
        'config_sha256': {str(p): hashlib.sha256(p.read_bytes()).hexdigest()
                          for p in paths if p.is_file()},
        'trading_config_count': len(CONFIG.get('kline', {}).get('trade_engine_configs', [])),
    }


def database_env():
    runtime = dict(entry.split(b'=', 1) for entry in
                   Path(f'/proc/{manager_pid()}/environ').read_bytes().split(b'\0')
                   if b'=' in entry)
    key = CONFIG.get('database', {}).get('url_env', 'CRYPTO_CTA_LOCAL_DATABASE_URL').encode()
    url = urllib.parse.urlsplit(runtime[key].decode())
    return dict(os.environ, PGHOST=url.hostname, PGPORT=str(url.port or 5432),
                PGUSER=urllib.parse.unquote(url.username),
                PGPASSWORD=urllib.parse.unquote(url.password or ''),
                PGDATABASE=urllib.parse.unquote(url.path.lstrip('/')), PGCONNECT_TIMEOUT='10')


def sql(query, env):
    result = subprocess.run([str(PG_BIN / 'psql'), '-X', '-q', '-A', '-t',
                             '-v', 'ON_ERROR_STOP=1'], input=query, text=True,
                            capture_output=True, env=env)
    if result.returncode:
        raise RuntimeError('verification database operation failed; credentials suppressed')
    return result.stdout.strip()
