# Append after remote_common.py and capture_body.py's setup before its try block.
try:
    baseline = json.loads((OUT / 'after.json').read_text())
    before = get('/manager/api/catalog/kline-status')
    assert before['target_history']['ready']
    results = []
    for source in ['binance_exec_trade03', 'binance_exec_trade05', 'binance_exec_trade06', 'binance_exec_trade07']:
        params = dict(window, maxPoints=300, sourceIds=source, includeTheoretical='true')
        began=time.monotonic()
        while True:
            body = get('/manager/api/timeline?' + urllib.parse.urlencode(params))
            if not any(t['loading'] for t in body['theoretical']):break
            emit('pending_repeat', {'source':source,'reasons':[t['unavailable_reason'] for t in body['theoretical']]})
            assert time.monotonic()-began<300
            time.sleep(5)
        old = baseline['queries'][source + '_timeline']
        assert body['report'] == old['report'], source + ' factual report changed'
        for actual, expected in zip(body['theoretical'], old['theoretical'], strict=True):
            assert actual['execution_model'] == expected['execution_model']
            assert actual['points'] == expected['points'], (source,actual['execution_model'],len(actual['points']),len(expected['points']),actual['unavailable_reason'])
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
    # This host-global counter also includes live background backfills. Keep
    # the four-source observation above, then measure one immediate warm hit.
    params=dict(window,maxPoints=300,sourceIds='binance_exec_trade07',includeTheoretical='true')
    warm_before=get('/manager/api/catalog/kline-status')
    warm=get('/manager/api/timeline?'+urllib.parse.urlencode(params))
    warm_after=get('/manager/api/catalog/kline-status')
    expected=baseline['queries']['binance_exec_trade07_timeline']
    assert warm['report']==expected['report']
    assert all(not t['loading'] and t['points']==e['points'] for t,e in zip(warm['theoretical'],expected['theoretical'],strict=True))
    emit('immediate_warm_hit',{'source':'binance_exec_trade07','request_delta':warm_after['requests']-warm_before['requests'],'active_before':warm_before['active_backfills'],'active_after':warm_after['active_backfills']})
    assert warm_after['requests']==warm_before['requests'],'immediate warm hit overlapped exchange requests'
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
