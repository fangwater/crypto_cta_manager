# Append after remote_common.py and capture_body.py's setup before its try block.
try:
    baseline = json.loads((OUT / 'after.json').read_text())
    before = get('/manager/api/catalog/kline-status')
    assert before['target_history']['ready']
    results = []
    for source in ['binance_exec_trade03', 'binance_exec_trade05', 'binance_exec_trade06', 'binance_exec_trade07']:
        params = dict(window, maxPoints=300, sourceIds=source, includeTheoretical='true')
        body = get('/manager/api/timeline?' + urllib.parse.urlencode(params))
        old = baseline['queries'][source + '_timeline']
        assert body['report'] == old['report'], source + ' factual report changed'
        for actual, expected in zip(body['theoretical'], old['theoretical'], strict=True):
            assert actual['execution_model'] == expected['execution_model']
            assert actual['points'] == expected['points']
            assert not actual['loading'] and actual['unavailable_reason'] is None
            assert actual['missing_price_count'] == 0
        results.append({'source': source, 'factual_unchanged': True, 'references_unchanged': True})
    after = get('/manager/api/catalog/kline-status')
    emit('cached_repeat', {
        'queries': results,
        'requests_before': before['requests'],
        'requests_after': after['requests'],
        'request_delta': after['requests'] - before['requests'],
        'target_history': after['target_history'],
    })
    assert after['requests'] == before['requests'], 'cached query initiated new requests'
    emit('health', get('/manager/api/health'))
    gateways = []
    for account in ['05', '06', '07']:
        for suffix in ['snapshot', 'config/']:
            path = '/exec_trade' + account + '/' + suffix
            with opener.open(urllib.request.Request('http://13.115.227.29:4191' + path, headers={'Cookie': 'cta_session=' + token}), timeout=30) as response:
                assert response.status == 200
                gateways.append({'path': path, 'status': response.status})
    emit('gateways', gateways)
finally:
    sql('DELETE FROM cta_auth_sessions WHERE session_id=' + session_id + ';', readonly=False)
    emit('diagnostic_session_removed', True)
