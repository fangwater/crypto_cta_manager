"""Appended to the host helper; exercise the real jp-meta Nginx gateway."""
import math

PRIVATE = {'binance_exec_trade06': '06', 'binance_exec_trade07': '07',
           'binance_exec_trade08': '08', 'binance_exec_rx04_5m05': '05'}
EXCLUDED = {'external:jp-meta-nav:gate_fr_arb04', 'external:jp-meta-nav:gate_fr_arb05'}
PUBLIC = {'binance_exec_trade01', 'binance_exec_trade02', 'binance_exec_trade03', 'binance_exec_trade04'}


def status(get, path, expected, method='GET', body=None):
    try:
        result = get(path, method, body)
        code = 200
    except urllib.error.HTTPError as error:
        code, result = error.code, None
    assert code == expected, (path, code, expected)
    emit('http', {'path': path, 'method': method, 'status': code})
    return result


for username in ['prc', 'lgq']:
    with session(username) as get:
        emit('checking', username)
        dashboard = get('dashboard')
        local_ids = {a['source_id'] for a in dashboard['accounts']}
        if username == 'prc':
            assert local_ids == PUBLIC, local_ids
        else:
            assert set(PRIVATE) <= local_ids
            for a in dashboard['accounts']:
                if a['source_id'] in PRIVATE:
                    assert a['access_level'] == 'configure'
        emit('local_accounts', {'username': username, 'ids': sorted(local_ids)})
        for route in ['auth/status', 'auth/users', 'catalog/virtual-accounts', 'catalog/position-updates?limit=5', 'symbols', 'nav/exchange', 'health']:
            data = get(route)
            if username == 'prc':
                serialized = json.dumps(data)
                assert all(source not in serialized for source in PRIVATE), route
        for source, suffix in PRIVATE.items():
            expected = 403 if username == 'prc' else 200
            for path in ['catalog/accounts/' + source, 'catalog/accounts/' + source + '/grants',
                         'symbols?sourceIds=' + source,
                         '/exec_trade' + suffix + '/snapshot', '/exec_trade' + suffix + '/config/']:
                status(get, path, expected)
            if username == 'prc':
                status(get, 'catalog/accounts/' + source + '/grants', 403, 'PUT', {'grants': []})
                status(get, 'timeline?sourceIds=' + source, 403)
                status(get, 'auth/users/1/sources', 403, 'PUT',
                       {'grants': [{'source_id': source, 'access_level': 'configure'}]})
        if username == 'prc':
            assert get('account-overview/external-access') == []
        overview = get('account-overview')
        for attempt in range(18):
            missing = [a['source_id'] for a in overview['accounts'] if
                       any(a.get(k) is None or not math.isfinite(a[k]) for k in ['equity', 'today_profit'])
                       or a['source_id'].startswith('external:') and a.get('collection_status') not in ['live', 'historical']]
            if not overview['loading'] and not missing and (username == 'prc' or len(overview['accounts']) == 19):
                break
            emit('overview_wait', {'username': username, 'count': len(overview['accounts']), 'missing': missing})
            time.sleep(10)
            overview = get('account-overview')
        assert not missing, missing
        ids = {a['source_id'] for a in overview['accounts']}
        assert not ids.intersection(EXCLUDED)
        if username == 'prc':
            assert ids == PUBLIC
            assert overview['external_status'] == []
        else:
            assert len(ids) == 19, ids
            assert set(PRIVATE) <= ids
            for upstream in ['external:jp-meta-nav:okex_mm_alpha', 'external:jp-meta-nav:bybit_mm_alpha',
                             'external:sg-cta:bybit_exec_trade01']:
                assert upstream in ids
        emit('overview_totals', {'username': username, 'count': len(ids),
             'equity': sum(a['equity'] for a in overview['accounts']),
             'today_profit': sum(a['today_profit'] for a in overview['accounts']),
             'generated_at_ms': overview['generated_at_ms'], 'missing': missing})
        if len(overview['dates']) > 1:
            historical = get('account-overview?date=' + overview['dates'][1])
            historical_ids = {a['source_id'] for a in historical['accounts']}
            assert not historical_ids.intersection(EXCLUDED)
            if username == 'prc':
                assert not historical_ids.intersection(PRIVATE)
                assert not any(s.startswith('external:') for s in historical_ids)
            emit('historical_ids', {'username': username, 'ids': sorted(historical_ids)})

with opener.open('http://127.0.0.1:4191/manager/api/symbols', timeout=20) as response:
    anonymous = json.load(response)
assert all(source not in json.dumps(anonymous) for source in PRIVATE)
for source in PRIVATE:
    try:
        opener.open('http://127.0.0.1:4191/manager/api/symbols?sourceIds=' + source, timeout=20)
        raise AssertionError('anonymous private symbols were exposed')
    except urllib.error.HTTPError as error:
        assert error.code == 403
emit('anonymous_private_symbols', 'denied')
emit('visibility_config', {'source_viewers': cfg['dashboard']['source_viewers'],
                          'excluded_source_ids': cfg['dashboard']['nav_overview']['excluded_source_ids']})
emit('grants_hash', sql("SELECT md5(coalesce(string_agg(to_jsonb(t)::text,E'\\n' ORDER BY to_jsonb(t)::text),'')) FROM cta_user_source_permissions t;"))
emit('protected', protected())
emit('service', subprocess.check_output(['systemctl', '--user', 'show', UNIT, '-p', 'MainPID', '-p', 'NRestarts', '-p', 'ActiveState'], text=True))
