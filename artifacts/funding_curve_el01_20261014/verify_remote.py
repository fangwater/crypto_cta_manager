"""Run via SSH stdin; emit sanitized UI/data/runtime verification only."""
import base64
import datetime
import hashlib
import http.client
import json
import math
import os
from pathlib import Path
import re
import subprocess
import time
import tomllib
import urllib.parse

root = Path('/home/el01/crypto_cta_manager')
config = tomllib.loads((root / 'config/cta-manager.toml').read_text())
source_id = 'binance_exec_trade09'
expected_runtime = globals().get('expected_runtime')
values = {}
for line in Path('/home/el01/.config/crypto-cta-manager/database.env').read_text().splitlines():
    line = line.strip().removeprefix('export ').strip()
    if line and not line.startswith('#') and '=' in line:
        key, value = line.split('=', 1)
        values[key] = value.strip().strip('"').strip("'")
url = urllib.parse.urlsplit(values[config['database']['url_env']])
db_env = os.environ.copy()
db_env.update(PGHOST=url.hostname, PGPORT=str(url.port),
              PGUSER=urllib.parse.unquote(url.username),
              PGPASSWORD=urllib.parse.unquote(url.password),
              PGDATABASE=url.path.lstrip('/'), PGCONNECT_TIMEOUT='5')


def emit(kind, **fields):
    print(json.dumps({'kind': kind, **fields}, ensure_ascii=False), flush=True)


def sql(query):
    result = subprocess.run(['/home/el01/.local/opt/postgresql/16/bin/psql',
                             '-X', '-A', '-t', '-v', 'ON_ERROR_STOP=1'],
                            input=query, env=db_env, capture_output=True, text=True)
    if result.returncode:
        raise RuntimeError('database verification failed')
    return result.stdout.strip()


def get(path, token=None):
    connection = http.client.HTTPConnection('127.0.0.1', 10051, timeout=90)
    try:
        connection.request('GET', path, headers={'Cookie': 'cta_session=' + token} if token else {})
        response = connection.getresponse()
        body = response.read()
        assert response.status == 200, (path, response.status)
        return body
    finally:
        connection.close()


def runtime_snapshot():
    processes = []
    for proc in Path('/proc').iterdir():
        if not proc.name.isdigit():
            continue
        try:
            executable = os.readlink(proc / 'exe')
            if not (executable.startswith('/home/el01/binance_exec_')
                    or executable.startswith('/home/el01/crypto_cta_manager/bin/')
                    or (Path(executable.removesuffix(' (deleted)')).name.startswith('python')
                        and b'/home/el01/binance_exec_' in (proc / 'cmdline').read_bytes())
                    or (Path(executable).name == 'nginx' and executable.startswith('/home/el01/'))):
                continue
            ticks = (proc / 'stat').read_text().rsplit(')', 1)[1].split()[19]
            processes.append({'pid': int(proc.name), 'executable': executable, 'start_ticks': ticks})
        except (OSError, IndexError):
            continue
    return {
        'processes': sorted(processes, key=lambda p: p['pid']),
        'unit_pids': {unit: subprocess.check_output(['systemctl', '--user', 'show', unit,
                      '--property=MainPID', '--value'], text=True).strip()
                      for unit in ['crypto-cta-manager-web.service', 'crypto-cta-nginx.service', 'postgresql-cta.service']},
        'config_sha256': hashlib.sha256((root / 'config/cta-manager.toml').read_bytes()).hexdigest(),
        'runtime_sha256': hashlib.sha256((root / 'bin/cta_web').read_bytes()).hexdigest(),
        'trade_configs': {path: hashlib.sha256(Path(path).read_bytes()).hexdigest()
                          for path in config['funding']['trade_engine_configs']},
    }


token = base64.urlsafe_b64encode(os.urandom(32)).decode().rstrip('=')
digest = hashlib.sha256(token.encode()).hexdigest()
session_id = sql("INSERT INTO cta_auth_sessions(user_id,token_hash,expires_at) "
                 "SELECT user_id,decode('" + digest + "','hex'),now()+interval '10 minutes' "
                 "FROM cta_users WHERE role='admin' AND disabled=false ORDER BY user_id LIMIT 1 "
                 "RETURNING session_id;").splitlines()[0]
assert session_id.isdigit()
try:
    dashboard = json.loads(get('/manager/api/dashboard', token))
    accounts = dashboard['accounts']
    defaults = {account['source_id']: 'account' if account['funding']['enabled']
                or account['funding']['start_ts_us'] is not None else 'strategy' for account in accounts}
    assert defaults[source_id] == 'account'
    assert [account['source_id'] for account in accounts if account['funding']['enabled']] == [source_id]
    emit('account_defaults', funding_source=source_id, funding_default=defaults[source_id],
         other_account_modes=sorted(set(mode for key, mode in defaults.items() if key != source_id)))
    end_ms = int(time.time() * 1000)
    for name, extra in [('1D', {'startMs': end_ms - 86400000}), ('ALL', {}),
                        ('1D_BTC', {'startMs': end_ms - 86400000, 'symbols': 'BTCUSDT'})]:
        query = urllib.parse.urlencode({'sourceIds': source_id, 'endMs': end_ms,
                                       'maxPoints': 3500, 'includeTheoretical': 'false', **extra})
        timeline = json.loads(get('/manager/api/account-timeline?' + query, token))
        report, funding = timeline['report'], timeline['funding']
        start_us, end_us = report['start_ts_us'], report['end_ts_us']
        symbol_filter = " AND symbol='BTCUSDT'" if name == '1D_BTC' else ''
        ledger = json.loads(sql("SELECT coalesce(json_agg(json_build_object('ts_us',ts_us,'income',income::double precision) ORDER BY ts_us,tran_id),'[]') "
                                "FROM cta_funding_income WHERE source_id='binance_exec_trade09' AND ts_us BETWEEN "
                                + str(start_us) + ' AND ' + str(end_us) + symbol_filter + ';'))
        expected_income = sum(row['income'] for row in ledger)
        assert math.isclose(funding['summary']['income_quote'], expected_income, abs_tol=1e-8)
        assert funding['summary']['record_count'] == len(ledger)
        points = funding['points']
        assert points and points[0]['ts_us'] == start_us and points[-1]['ts_us'] == end_us
        assert points[0]['funding_income_quote'] == 0
        actual_by_time = {point['ts_us']: point for point in report['points']}
        for point in points:
            actual = actual_by_time[point['ts_us']]
            expected = sum(row['income'] for row in ledger if row['ts_us'] <= point['ts_us'])
            if point['ts_us'] == start_us:
                expected = 0
            assert math.isclose(point['funding_income_quote'], expected, abs_tol=1e-8)
            for mode in ['before', 'after']:
                assert math.isclose(point[f'total_nav_{mode}_fee_quote'],
                                    actual[f'nav_change_{mode}_fee_quote'] + expected, abs_tol=1e-8)
        assert math.isclose(points[-1]['funding_income_quote'], expected_income, abs_tol=1e-8)
        emit('funding_curve', range=name, records=len(ledger), points=len(points), income_quote=expected_income,
             every_point_matches_ledger=True, zero_baseline=True, terminal_matches_summary=True,
             before_after_totals_additive=True)
    page = get('/manager/?source=' + source_id).decode()
    assets = re.findall(r'(?:src|href)="(/manager/assets/[^"]+)"', page)
    assert len(assets) >= 2
    for asset in assets:
        content = get(asset)
        if expected_runtime and asset.endswith('.js'):
            for text in ['Funding（累计资金费）', '区间 Funding（资金费）', '账户综合 PnL']:
                assert text.encode() in content, text
    emit('frontend_served', assets=assets, funding_labels_verified=bool(expected_runtime))
    current = runtime_snapshot()
    if expected_runtime:
        assert current == expected_runtime, 'protected runtime or trading configuration changed'
        emit('runtime_unchanged', processes_checked=len(current['processes']),
             trade_configs_checked=len(current['trade_configs']), manager_restarted=False)
    else:
        emit('runtime_baseline', snapshot=current)
    emit('verification_finished', frontend_release=os.path.realpath(root / 'webroot'),
         utc=datetime.datetime.now(datetime.timezone.utc).isoformat())
finally:
    sql('DELETE FROM cta_auth_sessions WHERE session_id=' + session_id + ';')
    assert sql('SELECT count(*) FROM cta_auth_sessions WHERE session_id=' + session_id + ';') == '0'
    emit('temporary_session_removed', removed=True)
