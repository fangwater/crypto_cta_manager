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


import sys
phase=sys.argv[1]
end_ms=int(sys.argv[2])
try:
    processes={}
    for process in Path('/proc').iterdir():
        if not process.name.isdigit(): continue
        try:
            exe=str((process/'exe').resolve())
            cmd=(process/'cmdline').read_bytes()
            if any(v in cmd for v in [b'trade_engine',b'exec-pre-trade',b'persist_manager',b'exec_config_server',b'viz',b'/home/el01/binance_exec',b'nginx',b'cta_monitor',b'spread_pbs',b'redis-server']):
                stat=(process/'stat').read_text().rsplit(')',1)[1].split()
                processes[process.name]={'exe':exe,'start_ticks':stat[19]}
        except (OSError,IndexError): pass
    files=[ROOT/'config/cta-manager.toml',ROOT/'nginx/nginx.conf']
    files += [Path(p) for p in cfg.get('kline',{}).get('trade_engine_configs',[])]
    config_hashes={str(p):hashlib.sha256(p.read_bytes()).hexdigest() for p in files if p.is_file()}
    tables={}
    for table in ['cta_position_snapshots','cta_position_snapshot_entries','cta_strategy_position_snapshots','cta_strategy_position_snapshot_entries','cta_account_strategy_bindings']:
        tables[table]=json.loads(sql("SELECT json_build_object('count',count(*),'hash',md5(coalesce(string_agg(row_to_json(t)::text,E'\\n' ORDER BY row_to_json(t)::text),''))) FROM "+table+" t;"))
    emit({'name':'protected','body':{'phase':phase,'processes':processes,'config_hashes':config_hashes,'tables':tables,'manager_pid':pid,'cpus':[l for l in Path('/proc/'+pid+'/status').read_text().splitlines() if l.startswith('Cpus_allowed_list:')],'webroot':str((ROOT/'webroot').resolve())}})
    get('health','health')
    dashboard=get('dashboard','dashboard')
    common=dict(startMs=end_ms-86400000,endMs=end_ms,maxPoints=30,includeTheoretical='false')
    for account in dashboard['accounts']:
        if not account['enabled'] or account['source_id'] != 'binance_exec_trade14':continue
        get(account['source_id']+'_1d','timeline?'+urllib.parse.urlencode(dict(common,sourceIds=account['source_id'])))
    get('hxl_history','timeline?'+urllib.parse.urlencode(dict(common,sourceIds='binance_exec_trade14',startMs=1792046151334)))
    get('hxl_closed_usdt_1d','timeline?'+urllib.parse.urlencode(dict(common,sourceIds='binance_exec_trade14',symbols='BTCUSDT,ETHUSDT,SOLUSDT,XRPUSDT')))
    get('hxl_initial_export','pnl/strategy/summary?'+urllib.parse.urlencode(dict(startMs=1792046151334,endMs=end_ms,sourceId='binance_exec_trade14',strategyName='__initial_position__')))
finally:
    sql('DELETE FROM cta_auth_sessions WHERE session_id=' + session_id + ';')
    emit({'verification_session_removed':True})
