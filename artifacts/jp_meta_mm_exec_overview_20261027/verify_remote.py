"""Appended to host_common.py; validates live data and both user boundaries."""
import math
import re

deadline = time.monotonic() + 600
while True:
    try:
        with opener.open('http://127.0.0.1:4191/manager/api/auth/status', timeout=10) as r:
            assert r.status == 200
        break
    except urllib.error.HTTPError as error:
        if error.code not in [502, 503, 504] or time.monotonic() > deadline:
            raise
        emit('startup_pending', {'host': HOST, 'status': error.code})
        time.sleep(10)


def export_get(token=True):
    headers = {'Authorization': 'Bearer ' + runtime[b'CRYPTO_SG_OVERVIEW_TOKEN'].decode()} if token else {}
    req = urllib.request.Request('http://127.0.0.1:4191/manager/api/integrations/account-overview', headers=headers)
    with opener.open(req, timeout=20) as r:
        return json.load(r)


if HOST == 'sg':
    exported = export_get()
    assert [r['source_id'] for r in exported['accounts']] == ['bybit_exec_trade01']
    assert exported['accounts'][0]['account_kind'] == 'cta'
    assert exported['accounts'][0]['collection_status'] == 'live'
    emit('sg_export', exported)
    try:
        export_get(False)
        raise AssertionError('anonymous export accepted')
    except urllib.error.HTTPError as error:
        assert error.code == 401
    with session('lgq') as get:
        try:
            get('integrations/account-overview')
            raise AssertionError('session-only export accepted')
        except urllib.error.HTTPError as error:
            assert error.code == 401
        for item in FIXED:
            actual = get(item['path'])
            previous = json.loads(json.dumps(item['response']['report']))
            actual_report = actual['report']
            retained = set(actual_report['available_strategies'])
            omitted = [series for series in previous['strategy_points'] if series['strategy'] not in retained]
            for series in omitted:
                assert series['strategy'] == 'system_position_close'
                assert series['symbol_count'] == 0
                assert series['gross_position_value_quote'] == series['net_position_value_quote'] == 0
                assert all(value == 0 for value in series['summary'].values())
                assert all(value == 0 for point in series['points'] for key, value in point.items() if key != 'ts_us')
            previous['available_strategies'] = [s for s in previous['available_strategies'] if s in retained]
            previous['strategy_points'] = [s for s in previous['strategy_points'] if s['strategy'] in retained]
            assert actual_report == previous, item['path']
            emit('omitted_empty_series', [s['strategy'] for s in omitted])
            emit('fixed_order_nav_unchanged', item['path'])
    emit('machine_export_auth', 'token required; anonymous and browser session denied')
else:
    # Both workers have independent HTTP/cache initialization.
    expected = {
        'external:jp-meta-nav:okex_mm_alpha', 'external:jp-meta-nav:bybit_mm_alpha',
        'external:sg-cta:bybit_exec_trade01',
    }
    query = "SELECT json_agg(slug ORDER BY slug) FROM strategy_envs WHERE enabled AND (strategy_kind='funding_rate' OR (strategy_kind='market_making' AND slug IN ('okex_mm_alpha','bybit_mm_alpha')));"
    result = subprocess.run(['psql','-h','/var/run/postgresql','-U','ubuntu','-d','crypto_nav_manager','-X','-qAt','-v','ON_ERROR_STOP=1'],
        input=query,text=True,capture_output=True,env=dict(os.environ,PGOPTIONS='-c default_transaction_read_only=on'))
    assert result.returncode == 0
    nav_ids = set(json.loads(result.stdout))
    expected_ids = {'external:jp-meta-nav:' + slug for slug in nav_ids} | {'external:sg-cta:bybit_exec_trade01'}
    with session('lgq') as get:
        while True:
            report = get('account-overview')
            external = [r for r in report['accounts'] if r['source_id'].startswith('external:')]
            ids = {r['source_id'] for r in external}
            if ids == expected_ids and all(r['collection_status'] == 'live' for r in external if r['source_id'] in expected):
                break
            assert time.monotonic() < deadline, 'external accounts not ready'
            emit('provider_pending', {'count': len(ids), 'states': report.get('external_status')})
            time.sleep(10)
        assert sum(r['account_kind'] == 'fr' for r in external) == len(nav_ids) - 2
        assert sum(r['account_kind'] == 'mm' for r in external) == 2
        for row in external:
            if row['source_id'] not in expected:
                continue
            for key in ['equity', 'long_position', 'short_position', 'net_position']:
                assert row[key] is not None and math.isfinite(row[key]), (row['source_id'], key)
            assert abs(row['net_position'] - row['long_position'] + row['short_position']) < 1e-8
        emit('lgq_overview', report)
    with session('prc') as get:
        for path in ['account-overview', 'account-overview?date=2026-10-26']:
            report = get(path)
            assert not any(r['source_id'].startswith('external:') for r in report['accounts'])
            assert not report.get('external_status')
            emit('prc_hidden', {'path': path, 'visible_local_source_ids': [r['source_id'] for r in report['accounts']]})
        assert get('account-overview/external-access') == []
        emit('prc_external_access_empty', True)
    nav = provider(cfg['dashboard']['nav_overview'])
    assert {row['source_id'] for row in nav['accounts']} == nav_ids
    emit('nav_provider', nav)
    emit('sg_provider', provider(cfg['dashboard']['external_overviews'][0]))
    with opener.open('http://127.0.0.1:4191/manager/', timeout=15) as r:
        html = r.read()
        assert r.status == 200
    for asset in re.findall(rb'(?:src|href)="([^\"]+\.(?:js|css))"', html):
        with opener.open('http://127.0.0.1:4191' + asset.decode(), timeout=15) as r:
            emit('asset', {'path': asset.decode(), 'sha256': hashlib.sha256(r.read()).hexdigest()})
emit('protected', protected())
emit('unit', subprocess.check_output(['systemctl', '--user', 'show', UNIT, '-p', 'MainPID', '-p', 'ActiveState', '-p', 'NRestarts'], text=True).splitlines())
