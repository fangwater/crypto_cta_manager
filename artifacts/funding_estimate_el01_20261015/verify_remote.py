"""Verify a Manager-only release; credentials and temporary sessions stay on host."""
import hashlib
import json
import math
import os
import pathlib
import secrets
import subprocess
import sys
import time
import tomllib
import urllib.error
import urllib.parse
import urllib.request

ROOT = pathlib.Path('/home/el01/crypto_cta_manager')
UNIT = pathlib.Path('/home/el01/.config/systemd/user/crypto-cta-manager-web.service')
STAGE = pathlib.Path(sys.argv[2])
MODE = sys.argv[1]
assert MODE in ('before', 'after')
assert STAGE.parent == ROOT / 'backups' and STAGE.name.startswith('funding_estimate_')
before = json.loads((STAGE / 'before.json').read_text()) if MODE == 'after' else None
pid = int(subprocess.check_output(['systemctl', '--user', 'show', 'crypto-cta-manager-web.service', '-p', 'MainPID', '--value'], text=True))
runtime = dict(x.split(b'=', 1) for x in pathlib.Path(f'/proc/{pid}/environ').read_bytes().split(b'\0') if b'=' in x)
url = urllib.parse.urlsplit(runtime[b'CRYPTO_CTA_LOCAL_DATABASE_URL'].decode())
env = dict(os.environ, PGHOST=url.hostname, PGPORT=str(url.port or 5432),
           PGUSER=urllib.parse.unquote(url.username), PGPASSWORD=urllib.parse.unquote(url.password or ''), PGDATABASE=url.path.lstrip('/'))


def sql(query):
    result = subprocess.run(['/home/el01/.local/opt/postgresql/16/bin/psql', '-X', '-qAt', '-v', 'ON_ERROR_STOP=1'], input=query, text=True, capture_output=True, env=env)
    assert result.returncode == 0, 'verification database query failed (details suppressed)'
    return result.stdout.strip()


def digest(data):
    return hashlib.sha256(data).hexdigest()


def canonical(data):
    return digest(json.dumps(data, sort_keys=True, separators=(',', ':')).encode())


def close(a, b):
    assert math.isclose(a, b, rel_tol=1e-10, abs_tol=1e-8), (a, b)


config = tomllib.loads((ROOT / 'config/cta-manager.toml').read_text())
paths = [ROOT / 'config/cta-manager.toml', ROOT / 'nginx/nginx.conf',
         pathlib.Path('/home/el01/.config/systemd/user/crypto-cta-nginx.service')]
paths.extend(pathlib.Path(path) for path in sorted(set(config.get('kline', {}).get('trade_engine_configs', []) + config.get('funding', {}).get('trade_engine_configs', []))))
sources = [source for source in config['sources'] if source.get('enabled', True)]
processes = {}
for proc in pathlib.Path('/proc').iterdir():
    if not proc.name.isdecimal() or int(proc.name) in (pid, os.getpid()):
        continue
    try:
        comm = (proc / 'comm').read_text().strip()
        cmd = (proc / 'cmdline').read_bytes()
        if not (any(word in comm.lower() for word in ('trade', 'exec', 'viz', 'nginx', 'cta_monitor', 'signal', 'factor', 'spread', 'account_mon', 'persist', 'redis-server'))
                or comm.startswith('python') and any(word in cmd for word in (b'exec_config_server', b'viz_server'))
                or comm == 'postgres' and b' -D ' in cmd.replace(b'\0', b' ')):
            continue
        processes[proc.name] = {'comm': comm, 'start_ticks': (proc / 'stat').read_text().rsplit(')', 1)[1].split()[19],
                               'cpu_affinity': sorted(os.sched_getaffinity(int(proc.name)))}
    except OSError:
        continue
tables = ['cta_position_snapshots', 'cta_position_snapshot_entries', 'cta_strategy_position_snapshots', 'cta_strategy_position_snapshot_entries']
state = {
    'captured_at_us': time.time_ns() // 1000,
    'manager_pid': pid,
    'manager_cpu_affinity': sorted(os.sched_getaffinity(pid)),
    'protected': processes,
    'config_sha256': {str(path): digest(path.read_bytes()) for path in paths if path.is_file()},
    'unit_semantics_sha256': canonical(sorted(line.strip() for line in UNIT.read_text().splitlines() if line.strip() and not line.lstrip().startswith('#'))),
    'snapshot_hashes': {table: sql(f"SELECT md5(coalesce(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text),'[]')::text) FROM {table} t;") for table in tables},
    'funding_settings': json.loads(sql("SELECT jsonb_agg(to_jsonb(t)) FROM (SELECT source_id,funding_enabled,funding_refresh_secs FROM cta_order_sources ORDER BY source_id) t;")),
    'monitor_sha256': digest((ROOT / 'bin/cta_monitor').read_bytes()),
    'webroot': os.readlink(ROOT / 'webroot'),
}
if before:
    for key in ('manager_cpu_affinity', 'config_sha256', 'unit_semantics_sha256', 'snapshot_hashes', 'funding_settings', 'monitor_sha256'):
        assert state[key] == before[key], key
    for proc, identity in before['protected'].items():
        assert processes.get(proc) == identity, ('protected process changed', proc, identity['comm'])
    assert pid != before['manager_pid']
    state['protected_unchanged'] = len(before['protected'])

token = secrets.token_urlsafe(32)
token_hash = digest(token.encode())
session = sql("INSERT INTO cta_auth_sessions(user_id,token_hash,expires_at) SELECT user_id,decode('" + token_hash + "','hex'),now()+interval '20 minutes' FROM cta_users WHERE role='admin' AND NOT disabled ORDER BY user_id LIMIT 1 RETURNING session_id;")
assert session.isdecimal()
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def get(path, raw=False):
    request = urllib.request.Request('http://127.0.0.1:10051' + path, headers={'Cookie': 'cta_session=' + token})
    try:
        with opener.open(request, timeout=175) as response:
            data = response.read()
            return data if raw else json.loads(data)
    except urllib.error.HTTPError as error:
        raise RuntimeError('verification HTTP ' + str(error.code) + ' at ' + path.split('?')[0]) from None


def route_status(path):
    request = urllib.request.Request('http://127.0.0.1:10051' + path, headers={'Cookie': 'cta_session=' + token})
    try:
        with opener.open(request, timeout=30) as response:
            response.read()
            return response.status
    except urllib.error.HTTPError as error:
        return error.code


try:
    for attempt in range(60):
        try:
            state['health'] = get('/manager/api/health')
            break
        except RuntimeError as error:
            if not before or not any(code in str(error) for code in ('HTTP 502 ', 'HTTP 503 ')) or attempt == 59:
                raise
            print('Manager startup: waiting for initial dashboard generation', file=sys.stderr, flush=True)
            time.sleep(5)
    assert state['health']['status'] == 'ok' and state['health']['last_refresh_error'] is None
    end_ms = before['end_ms'] if before else (time.time_ns() // 1_000_000 // 60_000 - 10) * 60_000
    start_ms = before['start_ms'] if before else end_ms - 86_400_000
    state.update(start_ms=start_ms, end_ms=end_ms, timelines={})
    if not before:
        state['gateways'] = {}
        for source in sources:
            if source.get('gateway_prefix'):
                for suffix in ('', 'snapshot', 'config/'):
                    path = source['gateway_prefix'] + '/' + suffix
                    state['gateways'][path] = route_status(path)

    for source_config in sources:
        source = source_config['id']
        for mode, endpoint in (('account', 'account-timeline'), ('strategy', 'timeline')):
            key = source + '/' + mode
            query_start = before['timelines'][key]['query_start_ms'] if before else start_ms
            params = dict(sourceIds=source, startMs=query_start, endMs=end_ms, maxPoints=200, includeTheoretical='false')
            query = urllib.parse.urlencode(params)
            path = '/manager/api/' + endpoint + '?' + query
            try:
                timeline = get(path)
            except RuntimeError as error:
                if before or 'HTTP 400 ' not in str(error):
                    raise
                params.pop('startMs')
                anchor = get('/manager/api/' + endpoint + '?' + urllib.parse.urlencode(params))['report']['earliest_start_ts_us']
                query_start = max(start_ms, anchor // 1000 + 1)
                params['startMs'] = query_start
                path = '/manager/api/' + endpoint + '?' + urllib.parse.urlencode(params)
                timeline = get(path)
            if before:
                for attempt in range(60):
                    estimate = timeline['estimated_funding']
                    if not estimate['loading']:
                        break
                    print(f'{source} {mode}: funding cache loading', file=sys.stderr, flush=True)
                    time.sleep(10)
                    timeline = get(path)
                assert estimate['income_quote'] is not None and estimate['unavailable_reason'] is None, (source, mode, estimate['unavailable_reason'])
            report = timeline['report']
            result = {'query_start_ms': query_start, 'report_sha256': canonical(report), 'summary': report['summary'],
                      'funding_enabled': timeline['funding']['summary']['enabled'],
                      'actual_income_quote': timeline['funding']['summary']['income_quote']}
            if before:
                assert result['report_sha256'] == before['timelines'][key]['report_sha256'], ('order NAV changed', key)
                close(result['actual_income_quote'], before['timelines'][key]['actual_income_quote'])
                estimate = timeline['estimated_funding']
                for fee in ('before', 'after'):
                    close(estimate[f'total_nav_{fee}_fee_quote'], report['summary'][f'nav_change_{fee}_fee_quote'] + estimate['income_quote'])
                assert estimate['points'][0]['funding_income_quote'] == 0
                close(estimate['points'][-1]['funding_income_quote'], estimate['income_quote'])
                originals = {row['strategy']: row for row in report['strategy_points']}
                close(sum(row['income_quote'] for row in estimate['strategies']), estimate['income_quote'])
                for row in estimate['strategies']:
                    original = originals[row['strategy']]
                    for fee in ('before', 'after'):
                        close(row[f'total_nav_{fee}_fee_quote'], original['summary'][f'nav_change_{fee}_fee_quote'] + row['income_quote'])
                        close(row['points'][-1][f'total_nav_{fee}_fee_quote'], row[f'total_nav_{fee}_fee_quote'])
                    assert row['points'][0]['funding_income_quote'] == 0
                    close(row['points'][-1]['funding_income_quote'], row['income_quote'])
                if mode == 'strategy':
                    assert not timeline['funding']['points']
                else:
                    assert result['funding_enabled'] == next(row['funding_enabled'] for row in state['funding_settings'] if row['source_id'] == source)
                    for fee in ('before', 'after'):
                        close(timeline['funding'][f'total_nav_{fee}_fee_quote'], report['summary'][f'nav_change_{fee}_fee_quote'] + result['actual_income_quote'])
                result.update(estimated_income_quote=estimate['income_quote'], income_by_asset=estimate['income_by_asset'],
                              strategy_count=len(estimate['strategies']), settlement_count=estimate['settlement_count'],
                              reconciliation=estimate['reconciliation'], zero_start_and_terminal_totals_verified=True,
                              selected_funding='actual' if mode == 'account' and result['funding_enabled'] else 'estimated')
            state['timelines'][key] = result
            print(f'{key}: verified', file=sys.stderr, flush=True)
    if before:
        manifest = {}
        for line in (ROOT / 'webroot/RELEASE-MANIFEST.txt').read_text().splitlines():
            k, v = line.split('=', 1)
            manifest[k] = v
        assert manifest['source_commit'].startswith('0c7e864')
        expected_release = json.loads((STAGE / 'expected.json').read_text())
        for name in ('cta_web', 'nav_rebuild', 'nav_snapshot', 'nav_strategy_snapshot'):
            assert digest((ROOT / 'bin' / name).read_bytes()) == manifest[name + '_sha256'] == expected_release['binaries'][name]
        frontend = {}
        for path in (ROOT / 'webroot/manager').rglob('*'):
            if path.is_file():
                name = path.relative_to(ROOT / 'webroot/manager').as_posix()
                expected = digest(path.read_bytes())
                assert digest(get('/manager/' + name, raw=True)) == expected == expected_release['frontend'][name], name
                frontend[name] = expected
        assert set(frontend) == set(expected_release['frontend'])
        state.update(release_manifest=manifest, frontend_sha256=frontend, gateways={})
        for path, expected_status in before['gateways'].items():
            code = route_status(path)
            assert code == expected_status, ('gateway changed', path, expected_status, code)
            state['gateways'][path] = code
        state['target_history'] = get('/manager/api/catalog/kline-status')['target_history']
        state['health'] = get('/manager/api/health')
        assert state['health']['status'] == 'ok' and state['health']['last_refresh_error'] is None
        assert int(subprocess.check_output(['systemctl', '--user', 'show', 'crypto-cta-manager-web.service', '-p', 'MainPID', '--value'], text=True)) == pid
finally:
    sql('DELETE FROM cta_auth_sessions WHERE session_id=' + session + " AND token_hash=decode('" + token_hash + "','hex');")
    assert sql("SELECT count(*) FROM cta_auth_sessions WHERE token_hash=decode('" + token_hash + "','hex');") == '0'
    state['temporary_session_removed'] = True
print(json.dumps(state, indent=2))
