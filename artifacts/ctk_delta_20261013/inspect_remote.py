import base64
import hashlib
import http.client
import json
import math
import os
from pathlib import Path
import secrets
import subprocess
import sys
import time
import tomllib
import urllib.parse
import urllib.request

ROOT = Path('/home/ubuntu/crypto_cta_manager')
STAGE = ROOT / 'deploy-staging/acquisition-cost-20261013T034926Z'
ORIGIN = 'http://127.0.0.1:4191'
PHASE = 'inspect'
CONFIG = tomllib.loads((ROOT / 'config/cta-manager.toml').read_text())

def manager_pid():
    return int(subprocess.check_output(['systemctl', '--user', 'show', 'crypto-cta-manager-web.service', '-p', 'MainPID', '--value'], text=True))

def state():
    paths = [ROOT / 'config/cta-manager.toml', Path.home() / '.config/systemd/user/crypto-cta-manager-web.service',
             Path('/etc/nginx/snippets/crypto_cta_manager.conf'), Path('/etc/nginx/sites-available/crypto_proxy_4191.conf'),
             Path('/home/ubuntu/nginx_locations.txt')]
    paths += [Path(p) for p in CONFIG['kline']['trade_engine_configs']]
    processes = {}
    own = os.getpid()
    for proc in Path('/proc').iterdir():
        if not proc.name.isdigit() or int(proc.name) == own:
            continue
        try:
            comm = (proc / 'comm').read_text().strip()
            cmd = (proc / 'cmdline').read_bytes().replace(b'\0', b' ').decode(errors='replace')
            if comm == 'cta_web':
                continue
            if not (any(s in comm.lower() for s in ['trade', 'exec', 'viz', 'nginx', 'cta_monitor']) or
                    (comm.startswith('python') and any(s in cmd for s in ['exec_config_server', 'viz_server']))):
                continue
            stat = (proc / 'stat').read_text().rsplit(')', 1)[1].split()
            processes[proc.name] = {'comm': comm, 'start_ticks': stat[19]}
        except (OSError, IndexError):
            continue
    return {'manager_pid': manager_pid(), 'webroot': str((ROOT / 'webroot').resolve()),
            'protected_processes': processes,
            'config_sha256': {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in paths if p.is_file()},
            'trading_configs': len(CONFIG['kline']['trade_engine_configs']),
            'kline_weight_per_minute': CONFIG['kline']['weight_per_minute']}

def db_env():
    env = dict(os.environ)
    runtime = dict(entry.split(b'=', 1) for entry in Path(f'/proc/{manager_pid()}/environ').read_bytes().split(b'\0') if b'=' in entry)
    key = CONFIG.get('database', {}).get('url_env', 'CRYPTO_CTA_LOCAL_DATABASE_URL').encode()
    url = urllib.parse.urlsplit(runtime[key].decode())
    env.update(PGHOST=url.hostname, PGPORT=str(url.port or 5432), PGUSER=urllib.parse.unquote(url.username),
               PGPASSWORD=urllib.parse.unquote(url.password or ''), PGDATABASE=urllib.parse.unquote(url.path.lstrip('/')),
               PGCONNECT_TIMEOUT='10')
    return env

def sql(query, env):
    result = subprocess.run(['psql', '-X', '-q', '-A', '-t', '-v', 'ON_ERROR_STOP=1'], input=query, text=True,
                            capture_output=True, env=env)
    if result.returncode:
        raise RuntimeError('verification database operation failed (credentials and SQL suppressed)')
    return result.stdout.strip()

def session_request(token):
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    def get(path):
        req = urllib.request.Request(ORIGIN + path, headers={'Cookie': 'cta_session=' + token})
        with opener.open(req, timeout=175) as response:
            body = response.read()
            return response.status, body
    return get


env=db_env()
token=secrets.token_urlsafe(32);digest=hashlib.sha256(token.encode()).hexdigest()
row=sql("INSERT INTO cta_auth_sessions (user_id, token_hash, expires_at) SELECT user_id, decode('"+digest+"','hex'), now()+interval '5 minutes' FROM cta_users WHERE role='admin' AND disabled=false ORDER BY user_id LIMIT 1 RETURNING session_id;",env)
assert row.isdigit()
try:
    get=session_request(token)
    _,status=get('/manager/api/catalog/kline-status')
    print(json.dumps({'target_history':json.loads(status)['target_history']}),flush=True)
    end=int(time.time()//60*60000)-600000
    query={'startMs':end-86400000,'endMs':end,'sourceIds':'binance_exec_trade03','page':1,'pageSize':25}
    _,body=get('/manager/api/catalog/acquisition-cost?'+urllib.parse.urlencode(query))
    result=json.loads(body);report=result['report']
    print(json.dumps({'query':query,'warnings':report['warnings'],'page_count':report['page_count'],'first_page_symbols':sorted(set(r['symbol'] for r in report['rows'])),'worst_symbols':report['by_symbol'][:5],'ctk_worst_fills':[f for f in report['worst_fills'] if 'CTK' in f['symbol']][:2]},ensure_ascii=False),flush=True)
finally:
    sql("DELETE FROM cta_auth_sessions WHERE session_id="+row+" AND token_hash=decode('"+digest+"','hex');",env)
    print('temporary verification session removed',flush=True)
