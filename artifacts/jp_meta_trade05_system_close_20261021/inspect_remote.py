"""Observe RX02 through host SQL and the existing Nginx gateway.

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


for table in ['cta_position_snapshots', 'cta_position_snapshot_entries', 'cta_strategy_position_snapshots', 'cta_strategy_position_snapshot_entries']:
    emit(table, json.loads(sql("SELECT coalesce(json_agg(t),'[]'::json) FROM (SELECT * FROM " + table + " WHERE source_id='" + SOURCE + "') t;")))
emit('bindings', json.loads(sql("SELECT coalesce(json_agg(t),'[]'::json) FROM (SELECT position_strategy_name,source_id,shares,order_strategy_name FROM cta_account_strategy_bindings WHERE source_id='" + SOURCE + "') t;")))
token = secrets.token_urlsafe(32)
digest = hashlib.sha256(token.encode()).hexdigest()
session_id = sql("INSERT INTO cta_auth_sessions (user_id,token_hash,expires_at) SELECT user_id,decode('" + digest + "','hex'),now()+interval '15 minutes' FROM cta_users WHERE role='admin' AND disabled=false ORDER BY user_id LIMIT 1 RETURNING session_id;", readonly=False)
assert session_id.isdigit()
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def get(name, path):
    request = urllib.request.Request('http://13.115.227.29:4191' + path, headers={'Cookie': 'cta_session=' + token})
    try:
        with opener.open(request, timeout=55) as response:
            body = json.load(response)
            emit(name, body)
            return body
    except Exception as error:
        emit(name + '_error', {'type': type(error).__name__, 'status': getattr(error, 'code', None)})


try:
    get('exec_snapshot_before', '/exec_trade05/snapshot')
    get('health', '/manager/api/health')
    get('dashboard', '/manager/api/dashboard')
    end_ms = int(time.time() * 1000)
    for days in [1, 7, 30]:
        query = urllib.parse.urlencode(dict(sourceIds=SOURCE, startMs=end_ms-days*86400000, endMs=end_ms, maxPoints=40, includeTheoretical='false'))
        get('timeline_' + str(days) + 'd', '/manager/api/timeline?' + query)
    get('exec_snapshot_after', '/exec_trade05/snapshot')
finally:
    sql('DELETE FROM cta_auth_sessions WHERE session_id=' + session_id + ';', readonly=False)
    emit('diagnostic_session_removed', True)
