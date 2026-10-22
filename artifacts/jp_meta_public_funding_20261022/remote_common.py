"""Verify Manager through host SQL and the existing Nginx gateway.

The only database mutation is a short-lived diagnostic auth session, removed
in finally. Credentials and the session value remain in process memory.
"""
import hashlib
import json
import os
from pathlib import Path
import secrets
import subprocess
import time
import tomllib
import urllib.parse
import urllib.request

ROOT = Path('/home/ubuntu/crypto_cta_manager')
SOURCE = 'binance_exec_trade05'
cfg = tomllib.loads((ROOT / 'config/cta-manager.toml').read_text())
pid = subprocess.check_output(['systemctl', '--user', 'show', 'crypto-cta-manager-web.service', '-p', 'MainPID', '--value'], text=True).strip()
runtime = dict(x.split(b'=', 1) for x in Path('/proc/' + pid + '/environ').read_bytes().split(b'\0') if b'=' in x)
u = urllib.parse.urlsplit(runtime[cfg.get('database', {}).get('url_env', 'CRYPTO_CTA_LOCAL_DATABASE_URL').encode()].decode())
env = dict(os.environ, PGHOST=u.hostname, PGPORT=str(u.port or 5432), PGUSER=urllib.parse.unquote(u.username), PGPASSWORD=urllib.parse.unquote(u.password or ''), PGDATABASE=urllib.parse.unquote(u.path.lstrip('/')), PGCONNECT_TIMEOUT='10', PGOPTIONS='-c statement_timeout=15000')


def sql(query, readonly=True):
    operation_env = dict(env)
    if readonly:
        operation_env['PGOPTIONS'] += ' -c default_transaction_read_only=on'
    result = subprocess.run(['psql', '-X', '-q', '-A', '-t', '-v', 'ON_ERROR_STOP=1'], input=query, text=True, capture_output=True, env=operation_env)
    if result.returncode:
        raise RuntimeError('diagnostic database operation failed; credentials suppressed')
    return result.stdout.strip()


def emit(name, body):
    print(json.dumps({'name': name, 'body': body}, ensure_ascii=False), flush=True)
