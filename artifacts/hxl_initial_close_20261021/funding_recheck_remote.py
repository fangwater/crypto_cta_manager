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
import urllib.error

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
                 "','hex'),now()+interval '30 minutes' FROM cta_users WHERE role='admin' AND disabled=false ORDER BY user_id LIMIT 1 RETURNING session_id;")
assert session_id.isdigit()
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def emit(value):
    print(json.dumps(value, ensure_ascii=False), flush=True)


def get(name, path):
    began=time.monotonic()
    while True:
        request=urllib.request.Request('http://127.0.0.1:10051/manager/api/'+path,headers={'Cookie':'cta_session='+token})
        try:
            with opener.open(request,timeout=55) as response:
                result=json.load(response)
                emit({'name':name,'elapsed_s':round(time.monotonic()-began,3),'http_status':response.status,'body':result})
                return result
        except urllib.error.HTTPError as error:
            try: payload=json.loads(error.read())
            except ValueError: payload={'error':'gateway error'}
            emit({'progress':name,'http_status':error.code,'body':payload})
            if error.code in [502,503,504] and time.monotonic()-began<300:
                time.sleep(10)
                continue
            raise RuntimeError('diagnostic query failed: '+name+' HTTP '+str(error.code))


try:
    common=dict(startMs=1792565280000-86400000,endMs=1792565280000,maxPoints=200,includeTheoretical='false')
    for source in ['binance_exec_trade01','binance_exec_trade02','binance_exec_trade03','binance_exec_trade06','binance_exec_trade07']:
        get(source+'_1d','timeline?'+urllib.parse.urlencode(dict(common,sourceIds=source)))
finally:
    sql('DELETE FROM cta_auth_sessions WHERE session_id=' + session_id + ';')
    emit({'verification_session_removed':True})
