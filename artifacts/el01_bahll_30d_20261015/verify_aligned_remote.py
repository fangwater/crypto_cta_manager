"""Diagnose read-only dashboard queries; keep credentials on the host."""
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
UNIT = 'crypto-cta-manager-web.service'
cfg = tomllib.loads((ROOT / 'config/cta-manager.toml').read_text())
pid = subprocess.check_output(['systemctl', '--user', 'show', UNIT, '-p', 'MainPID', '--value'], text=True).strip()
runtime = dict(entry.split(b'=', 1) for entry in Path(f'/proc/{pid}/environ').read_bytes().split(b'\0') if b'=' in entry)
url = urllib.parse.urlsplit(runtime[cfg.get('database', {}).get('url_env', 'CRYPTO_CTA_LOCAL_DATABASE_URL').encode()].decode())
env = dict(os.environ, PGHOST=url.hostname, PGPORT=str(url.port or 5432),
           PGUSER=urllib.parse.unquote(url.username), PGPASSWORD=urllib.parse.unquote(url.password or ''),
           PGDATABASE=urllib.parse.unquote(url.path.lstrip('/')), PGCONNECT_TIMEOUT='10')


def sql(query):
    result = subprocess.run([str(Path.home() / '.local/opt/postgresql/16/bin/psql'), '-X', '-q', '-A', '-t', '-v', 'ON_ERROR_STOP=1'],
                            input=query, text=True, capture_output=True, env=env)
    if result.returncode:
        raise RuntimeError('diagnostic database operation failed (credentials suppressed)')
    return result.stdout.strip()


token = secrets.token_urlsafe(32)
digest = hashlib.sha256(token.encode()).hexdigest()
session_id = sql("INSERT INTO cta_auth_sessions (user_id,token_hash,expires_at) SELECT user_id,decode('" + digest +
                 "','hex'),now()+interval '10 minutes' FROM cta_users WHERE role='admin' AND disabled=false ORDER BY user_id LIMIT 1 RETURNING session_id;")
assert session_id.isdigit()
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def emit(value):
    print(json.dumps(value, ensure_ascii=False), flush=True)


def get(name, path):
    began = time.monotonic()
    request = urllib.request.Request('http://127.0.0.1:10051/manager/api/' + path,
                                     headers={'Cookie': 'cta_session=' + token})
    with opener.open(request, timeout=55) as response:
        raw = response.read()
        result = json.loads(raw)
        emit({'name': name, 'elapsed_s': round(time.monotonic() - began, 3),
              'bytes': len(raw), 'http_status': response.status, 'body': result})
    return result


try:
    get('health_before', 'health')
    get('status_before', 'catalog/kline-status')
    end_ms = int(time.time() * 1000)
    common = dict(startMs=end_ms-30*86400000, endMs=end_ms,
                  sourceIds='binance_exec_trade01', maxPoints=3500)
    boundary = get('rejected_30d', 'timeline?' + urllib.parse.urlencode(dict(common, includeTheoretical='true')))
    common['startMs'] = (boundary['theoretical']['available_from_us']+999)//1000 + boundary['theoretical']['execution_window_secs']*1000
    for attempt in range(18):
        result = get('aligned_30d_'+str(attempt), 'timeline?' + urllib.parse.urlencode(dict(common, includeTheoretical='true')))
        theory = result['theoretical']
        available = (theory['available_from_us']+999)//1000
        if common['startMs'] < available:
            common['startMs'] = available+theory['execution_window_secs']*1000
            continue
        if theory['points']:
            get('aligned_repeat', 'timeline?' + urllib.parse.urlencode(dict(common, includeTheoretical='true')))
            break
        if not theory['loading']:
            break
        time.sleep(10)
    get('status_after', 'catalog/kline-status')
    get('health_after', 'health')

finally:
    sql('DELETE FROM cta_auth_sessions WHERE session_id=' + session_id + ';')
    emit({'verification_session_removed': True})
