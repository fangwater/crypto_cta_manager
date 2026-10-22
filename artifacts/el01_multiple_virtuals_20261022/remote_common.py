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

ROOT = Path('/home/el01/crypto_cta_manager')
SOURCE = 'binance_exec_trade01'
cfg = tomllib.loads((ROOT / 'config/cta-manager.toml').read_text())
pid = subprocess.check_output(['systemctl', '--user', 'show', 'crypto-cta-manager-web.service', '-p', 'MainPID', '--value'], text=True).strip()
runtime = dict(x.split(b'=', 1) for x in Path('/proc/' + pid + '/environ').read_bytes().split(b'\0') if b'=' in x)
u = urllib.parse.urlsplit(runtime[cfg.get('database', {}).get('url_env', 'CRYPTO_CTA_LOCAL_DATABASE_URL').encode()].decode())
env = dict(os.environ, PATH='/home/el01/.local/opt/postgresql/16/bin:'+os.environ.get('PATH',''), PGHOST=u.hostname, PGPORT=str(u.port or 5432), PGUSER=urllib.parse.unquote(u.username), PGPASSWORD=urllib.parse.unquote(u.password or ''), PGDATABASE=urllib.parse.unquote(u.path.lstrip('/')), PGCONNECT_TIMEOUT='10', PGOPTIONS='-c statement_timeout=15000')


def sql(query, readonly=True):
    operation_env = dict(env)
    if readonly:
        operation_env['PGOPTIONS'] += ' -c default_transaction_read_only=on'
    result = subprocess.run(['/home/el01/.local/opt/postgresql/16/bin/psql', '-X', '-q', '-A', '-t', '-v', 'ON_ERROR_STOP=1'], input=query, text=True, capture_output=True, env=operation_env)
    if result.returncode:
        raise RuntimeError('diagnostic database operation failed; credentials suppressed')
    return result.stdout.strip()


def emit(name, body):
    print(json.dumps({'name': name, 'body': body}, ensure_ascii=False), flush=True)

import contextlib
import shutil
import sys
import urllib.error

UNIT = 'crypto-cta-manager-web.service'
WORK = ROOT / 'backups/multiple_virtuals_20261022'
CONFIG = ROOT / 'config/cta-manager.toml'
SOURCES = [s['id'] for s in cfg['sources'] if s.get('enabled', True)]
TABLES = ['cta_position_snapshots', 'cta_position_snapshot_entries',
          'cta_strategy_position_snapshots', 'cta_strategy_position_snapshot_entries']


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def protected():
    state = {'manager_pid': pid, 'binary_sha256': hashlib.sha256((ROOT/'bin/cta_web').read_bytes()).hexdigest(),
             'webroot': str((ROOT/'webroot').resolve()), 'processes': {}, 'config_hashes': {}, 'tables': {}}
    for process in Path('/proc').iterdir():
        if not process.name.isdigit():
            continue
        try:
            command = (process/'cmdline').read_bytes()
            if any(name in command for name in [b'trade_engine', b'exec-pre-trade', b'persist_manager',
                    b'exec_config_server', b'viz_server', b'account_monitor', b'nginx', b'cta_monitor',
                    b'spread_pbs', b'redis-server', b'cta_perp', b'cta_spread']):
                state['processes'][process.name] = {
                    'exe': str((process/'exe').resolve()),
                    'start_ticks': (process/'stat').read_text().rsplit(')', 1)[1].split()[19],
                    'affinity': next(line for line in (process/'status').read_text().splitlines()
                                     if line.startswith('Cpus_allowed_list:'))}
        except (OSError, IndexError):
            continue
    files = [ROOT/'nginx/nginx.conf', Path('/home/el01/.config/systemd/user/crypto-cta-manager-web.service')]
    files.extend(Path(p) for p in cfg.get('kline', {}).get('trade_engine_configs', []))
    for path in files:
        if path.is_file():
            state['config_hashes'][str(path)] = hashlib.sha256(path.read_bytes()).hexdigest()
    for table in TABLES:
        state['tables'][table] = json.loads(sql("SELECT json_build_object('count',count(*),'hash',md5(coalesce(string_agg(row_to_json(t)::text,E'\\n' ORDER BY row_to_json(t)::text),''))) FROM " + table + ' t;'))
    state['funding_settings'] = json.loads(sql("SELECT json_agg(t ORDER BY source_id) FROM (SELECT source_id,funding_enabled,funding_refresh_secs FROM cta_order_sources) t;"))
    state['manager_affinity'] = next(line for line in Path('/proc/'+pid+'/status').read_text().splitlines() if line.startswith('Cpus_allowed_list:'))
    return state


@contextlib.contextmanager
def gateway():
    token = secrets.token_urlsafe(32)
    token_hash = hashlib.sha256(token.encode()).hexdigest()
    session = sql("INSERT INTO cta_auth_sessions (user_id,token_hash,expires_at) SELECT user_id,decode('" + token_hash + "','hex'),now()+interval '30 minutes' FROM cta_users WHERE role='admin' AND disabled=false ORDER BY user_id LIMIT 1 RETURNING session_id;", readonly=False)
    assert session.isdigit()
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    def get(path):
        request = urllib.request.Request('http://127.0.0.1:10051/manager/api/' + path,
                                         headers={'Cookie': 'cta_session=' + token})
        began = time.monotonic()
        while True:
            try:
                with opener.open(request, timeout=55) as response:
                    return json.load(response)
            except urllib.error.HTTPError as error:
                if path != 'health' or error.code not in [502, 503, 504] or time.monotonic()-began > 600:
                    raise
                emit('startup_pending', {'http': error.code})
                time.sleep(10)

    try:
        yield get
    finally:
        sql('DELETE FROM cta_auth_sessions WHERE session_id=' + session + ';', readonly=False)
        assert sql('SELECT count(*) FROM cta_auth_sessions WHERE session_id=' + session + ';') == '0'
        emit('diagnostic_session_removed', True)
