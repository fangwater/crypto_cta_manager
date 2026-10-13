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

ROOT = Path(sys.argv[2])
STAGE = ROOT / 'deploy-staging/analysis-switch-20261013T053157Z'
ORIGIN = sys.argv[3]
PHASE = sys.argv[1]
CONFIG = tomllib.loads((ROOT / 'config/cta-manager.toml').read_text())

def manager_pid():
    return int(subprocess.check_output(['systemctl', '--user', 'show', 'crypto-cta-manager-web.service', '-p', 'MainPID', '--value'], text=True))

def state():
    paths = [ROOT / 'config/cta-manager.toml', Path.home() / '.config/systemd/user/crypto-cta-manager-web.service',
             Path('/etc/nginx/snippets/crypto_cta_manager.conf'), Path('/etc/nginx/sites-available/crypto_proxy_4191.conf'),
             Path('/home/ubuntu/nginx_locations.txt')]
    paths += [ROOT / 'nginx/nginx.conf']
    paths += [Path(p) for p in CONFIG['kline']['trade_engine_configs']]
    for source in CONFIG['sources']:
        if source.get('trade_engine_config_path'): paths.append(Path(source['trade_engine_config_path']))
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
    psql = str(Path.home() / '.local/opt/postgresql/16/bin/psql') if (Path.home() / '.local/opt/postgresql/16/bin/psql').exists() else 'psql'
    result = subprocess.run([psql, '-X', '-q', '-A', '-t', '-v', 'ON_ERROR_STOP=1'], input=query, text=True,
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


def inspect():
    current = state()
    STAGE.mkdir(parents=True, exist_ok=True)
    (STAGE / (PHASE + '_state.json')).write_text(json.dumps(current, indent=2))
    env = db_env()
    token = secrets.token_urlsafe(32)
    token_hash = hashlib.sha256(token.encode()).hexdigest()
    row = sql("INSERT INTO cta_auth_sessions (user_id, token_hash, expires_at) SELECT user_id, decode('" + token_hash + "','hex'), now() + interval '10 minutes' FROM cta_users WHERE role='admin' AND disabled=false ORDER BY user_id LIMIT 1 RETURNING session_id;", env)
    assert row.isdigit()
    get = session_request(token)
    try:
        status_code, body = get('/manager/api/catalog/kline-status')
        status = json.loads(body)
        print(json.dumps({'phase':PHASE,'pid':current['manager_pid'],'kline_status':status,'protected_processes':len(current['protected_processes'])}, ensure_ascii=False), flush=True)
        if PHASE == 'jp_status': return
        if PHASE == 'before':
            end = int(time.time() // 60 * 60000) - 600000
            query = {'sourceIds': 'binance_exec_trade01', 'startMs':end-3600000,'endMs':end,'includeTheoretical':'false','maxPoints':200}
            (STAGE / 'query.json').write_text(json.dumps(query))
        else:
            query = json.loads((STAGE / 'query.json').read_text())
        code, body = get('/manager/api/timeline?' + urllib.parse.urlencode(query))
        timeline = json.loads(body)
        (STAGE / (PHASE + '_timeline.json')).write_text(json.dumps(timeline))
        summary = {'manager_pid':current['manager_pid'],'kline_enabled':status['enabled'],'target_history':status['target_history'], 'factual_timeline_http':code}
        if PHASE == 'after':
            previous = json.loads((STAGE / 'before_state.json').read_text())
            assert previous['protected_processes'] == current['protected_processes'], 'protected process changed'
            old_hashes = previous['config_sha256'].copy(); new_hashes = current['config_sha256'].copy()
            del old_hashes[str(ROOT / 'config/cta-manager.toml')]; del new_hashes[str(ROOT / 'config/cta-manager.toml')]
            assert old_hashes == new_hashes, 'protected config changed'
            old_config = tomllib.loads((STAGE / 'cta-manager.toml.previous').read_text())
            old_config['kline']['enabled'] = False
            assert old_config == CONFIG, 'unexpected Manager config change'
            old_timeline = json.loads((STAGE / 'before_timeline.json').read_text())
            assert old_timeline['report'] == timeline['report'], 'fixed factual NAV changed'
            assert not status['enabled'] and status['requests'] == 0 and status['active_backfills'] == 0
            target = status['target_history']
            assert not target['enabled'] and not target['loading'] and not target['ready'] and target['processed_messages'] == 0
            assert not timeline['theoretical']['loading'] and '已关闭' in timeline['theoretical']['unavailable_reason']
            query['includeTheoretical']='true'
            _, full = get('/manager/api/timeline?' + urllib.parse.urlencode(query))
            full = json.loads(full)
            assert full['report'] == timeline['report'] and not full['theoretical']['loading']
            for attempt in range(2):
                try:
                    get('/manager/api/catalog/acquisition-cost?sourceIds=binance_exec_trade01')
                    raise AssertionError('disabled cost query unexpectedly succeeded')
                except urllib.error.HTTPError as error:
                    assert error.code == 400 and '已关闭' in error.read().decode()
            _, status_after = get('/manager/api/catalog/kline-status')
            status_after=json.loads(status_after)
            assert status_after['target_history']['processed_messages'] == 0 and status_after['requests'] == 0
            checks={}
            for route in ['/manager/api/health','/manager/api/dashboard','/exec_trade01/','/exec_trade01/snapshot','/exec_trade01/config/']:
                checks[route]=get(route)[0]
            summary.update(protected_processes_unchanged=len(current['protected_processes']), protected_configs_unchanged=True, manager_config_only_kline_disabled=True, factual_nav_unchanged=True, disabled_queries_do_not_start_work=True, gateway_http=checks)
        (STAGE / (PHASE + '_summary.json')).write_text(json.dumps(summary, indent=2, ensure_ascii=False))
        print(json.dumps(summary,ensure_ascii=False),flush=True)
    finally:
        sql("DELETE FROM cta_auth_sessions WHERE session_id=" + row + " AND token_hash=decode('" + token_hash + "','hex');", env)
        assert sql('SELECT count(*) FROM cta_auth_sessions WHERE session_id=' + row + ';',env)=='0'
        print('temporary verification session removed',flush=True)
inspect()
