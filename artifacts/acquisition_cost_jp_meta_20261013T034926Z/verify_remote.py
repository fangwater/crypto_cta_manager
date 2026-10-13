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
PHASE = sys.argv[1]
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

def verify():
    current = state()
    if PHASE == 'before':
        (STAGE / 'before_state.json').write_text(json.dumps(current, indent=2))
        end = int(time.time() // 60 * 60000) - 600000
        query = {'startMs': end - 86400000, 'endMs': end, 'sourceIds': 'binance_exec_trade03', 'page': 1, 'pageSize': 25}
        (STAGE / 'query.json').write_text(json.dumps(query))
    else:
        previous = json.loads((STAGE / 'before_state.json').read_text())
        assert current['config_sha256'] == previous['config_sha256'], 'protected config changed'
        assert current['protected_processes'] == previous['protected_processes'], 'protected processes changed'
        assert current['manager_pid'] != previous['manager_pid'], 'Manager did not restart'
        (STAGE / 'after_state.json').write_text(json.dumps(current, indent=2))
        query = json.loads((STAGE / 'query.json').read_text())
    print(json.dumps({'phase': PHASE, 'manager_pid': current['manager_pid'], 'protected_processes': len(current['protected_processes']),
                      'trading_configs': current['trading_configs'], 'kline_weight_per_minute': current['kline_weight_per_minute']}), flush=True)
    env = db_env()
    token = secrets.token_urlsafe(32)
    token_hash = hashlib.sha256(token.encode()).hexdigest()
    row = sql("INSERT INTO cta_auth_sessions (user_id, token_hash, expires_at) SELECT user_id, decode('" + token_hash +
              "','hex'), now() + interval '40 minutes' FROM cta_users WHERE role='admin' AND disabled=false ORDER BY user_id LIMIT 1 RETURNING session_id;", env)
    assert row.isdigit(), 'no enabled verification administrator'
    try:
        get = session_request(token)
        statuses = {}
        for path in ['/manager/api/health', '/manager/api/dashboard', '/exec_trade03/', '/exec_trade03/snapshot', '/exec_trade03/config/']:
            status, _ = get(path)
            statuses[path] = status
            assert status == 200
        connection = http.client.HTTPConnection('127.0.0.1', 4191, timeout=15)
        connection.request('GET', '/exec_trade03/ws', headers={'Cookie': 'cta_session=' + token, 'Upgrade': 'websocket',
                           'Connection': 'Upgrade', 'Sec-WebSocket-Version': '13', 'Sec-WebSocket-Key': base64.b64encode(secrets.token_bytes(16)).decode()})
        response = connection.getresponse()
        statuses['/exec_trade03/ws'] = response.status
        connection.close()
        assert response.status == 101
        endpoint = '/manager/api/catalog/acquisition-cost?' + urllib.parse.urlencode(query)
        report = None
        for attempt in range(90):
            _, raw = get(endpoint)
            snapshot = json.loads(raw)
            report = snapshot['report']
            totals = report['totals']
            if report['rows'] and totals['matched_fill_count'] > 0 and totals['missing_virtual_delta_count'] == 0 and not report['warnings']:
                break
            print(json.dumps({'phase': PHASE, 'attempt': attempt + 1, 'rows': len(report['rows']), 'matched_fills': totals['matched_fill_count'],
                              'missing_deltas': totals['missing_virtual_delta_count'], 'warning_count': len(report['warnings'])}), flush=True)
            time.sleep(10)
        else:
            raise RuntimeError('cost report did not become complete')
        (STAGE / f'{PHASE}_cost.json').write_text(json.dumps(snapshot))
        summary = {'phase': PHASE, 'statuses': statuses, 'query': query, 'rows': len(report['rows']), 'matched_fills': totals['matched_fill_count'],
                   'virtual_deltas': totals['virtual_delta_count'], 'missing_deltas': totals['missing_virtual_delta_count'],
                   'generation_duration_ms': snapshot['generation_duration_ms'], 'protected_processes': len(current['protected_processes']),
                   'trading_configs': current['trading_configs']}
        if PHASE == 'after':
            old = json.loads((STAGE / 'before_cost.json').read_text())['report']
            stripped = json.loads(json.dumps(report))
            for r in stripped['rows']:
                r.pop('execution')
            for obj in [old, stripped]:
                obj.pop('generated_at_us', None)
            assert old == stripped, 'existing cost report changed for fixed window'
            points = 0
            for r in report['rows']:
                e = r['execution']
                assert len(e['fills']) <= 200
                assert [f['fill_ts_us'] for f in e['fills']] == sorted(f['fill_ts_us'] for f in e['fills'])
                points += len(e['fills'])
                if r['matched_fill_count']:
                    assert e['first_fill_ts_us'] is not None and e['last_fill_ts_us'] is not None
                    assert e['qty_weighted_target_delay_us'] is not None
                    assert 0 <= e['after_window_qty_ratio'] <= 1
                if not e['fills_sampled']:
                    assert len(e['fills']) == r['matched_fill_count']
                    qty = sum(abs(f['actual_qty']) for f in e['fills'])
                    if qty:
                        assert math.isclose(sum(abs(f['actual_qty']) * f['actual_price'] for f in e['fills']) / qty,
                                            r['actual_vwap'], rel_tol=1e-10)
                        assert math.isclose(sum(abs(f['actual_qty']) * f['target_delay_us'] for f in e['fills']) / qty,
                                            e['qty_weighted_target_delay_us'], rel_tol=1e-10)
                        late = sum(abs(f['actual_qty']) for f in e['fills'] if f['fill_ts_us'] >= r['virtual_execution_ts_us']) / qty
                        assert math.isclose(late, e['after_window_qty_ratio'], rel_tol=1e-10, abs_tol=1e-12)
            assert points > 0
            summary.update(cost_report_unchanged=True, chart_fill_points=points, protected_state_unchanged=True)
        (STAGE / f'{PHASE}_summary.json').write_text(json.dumps(summary, indent=2))
        print(json.dumps(summary), flush=True)
    finally:
        sql("DELETE FROM cta_auth_sessions WHERE session_id=" + row + " AND token_hash=decode('" + token_hash + "','hex');", env)
        assert sql('SELECT count(*) FROM cta_auth_sessions WHERE session_id=' + row + ';', env) == '0'
        print('temporary verification session removed', flush=True)

verify()
