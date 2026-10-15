"""Exercise ownership through Nginx; never submit targets to a bound strategy."""
import time

phase = sys.argv[1]
env = database_env()
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
sessions = {}
created_names = []
results = {'phase': phase}
prefix = 'owner_check_' + RELEASE + '_'


def call(user, path, method='GET', body=None):
    headers = {'Cookie': 'cta_session=' + sessions[user]['token']} if user else {}
    data = None
    if body is not None:
        headers['Content-Type'] = 'application/json'
        data = json.dumps(body).encode()
    request = urllib.request.Request('http://127.0.0.1:10051/manager/api/' + path,
                                     data=data, headers=headers, method=method)
    try:
        with opener.open(request, timeout=90) as response:
            raw = response.read()
            return response.status, json.loads(raw) if raw else None
    except urllib.error.HTTPError as error:
        raw = error.read()
        return error.code, json.loads(raw) if raw else None


try:
    for username in ['shaokai', 'dzy', 'admin']:
        token = secrets.token_urlsafe(32)
        token_hash = hashlib.sha256(token.encode()).hexdigest()
        where = "role='admin'" if username == 'admin' else "username='" + username + "'"
        row = sql("INSERT INTO cta_auth_sessions (user_id,token_hash,expires_at) "
                  "SELECT user_id,decode('" + token_hash + "','hex'),now()+interval '15 minutes' "
                  "FROM cta_users WHERE disabled=false AND " + where +
                  " ORDER BY user_id LIMIT 1 RETURNING session_id,user_id;", env)
        session_id, user_id = map(int, row.split('|'))
        sessions[username] = {'token': token, 'session_id': session_id, 'user_id': user_id}

    owned = json.loads(sql("SELECT coalesce(json_agg(strategy_name ORDER BY strategy_name),'[]') "
                           "FROM cta_position_strategies WHERE created_by_user_id=" +
                           str(sessions['shaokai']['user_id']) + ";", env))
    assert owned
    code, access = call('shaokai', 'catalog/position-strategies-access')
    assert code == 200
    accessible = {entry['strategy_name'] for entry in access}
    results['existing_owned_count'] = len(owned)
    results['existing_owned_configurable_count'] = sum(name in accessible for name in owned)
    assert results['existing_owned_configurable_count'] == (0 if phase == 'before' else len(owned))
    code, catalog = call('shaokai', 'catalog/position-strategies')
    assert code == 200
    assert all(next(entry for entry in catalog if entry['strategy_name'] == name)['can_publish'] for name in owned)
    results['existing_owned_editable'] = True

    if phase == 'after':
        for username in ['shaokai', 'dzy']:
            name = prefix + username
            assert sql("SELECT count(*) FROM cta_position_strategies WHERE strategy_name='" + name + "';", env) == '0'
            created_names.append(name)
            code, saved = call(username, 'catalog/position-strategies', 'POST',
                               {'strategy_name': name, 'targets': {}})
            assert code == 200 and saved['publishes'] == []
            code, access = call(username, 'catalog/position-strategies-access')
            assert code == 200 and any(entry['strategy_name'] == name for entry in access)
            other = 'dzy' if username == 'shaokai' else 'shaokai'
            code, catalog = call(other, 'catalog/position-strategies')
            assert code == 200 and all(entry['strategy_name'] != name for entry in catalog)
            assert call(other, 'catalog/position-strategies', 'POST',
                        {'strategy_name': name, 'targets': {}})[0] == 403
            path = 'catalog/position-strategies/' + urllib.parse.quote(name) + '/grants'
            assert call(other, path, 'PUT', {'grants': [], 'open_visibility': False})[0] == 403
            # Ownership survives replacing the explicit grant list with none.
            assert call(username, path, 'PUT', {'grants': [], 'open_visibility': False})[0] == 200
            assert call(username, path, 'PUT', {'grants': [], 'open_visibility': True})[0] == 403
            assert call(username, 'catalog/position-strategies/' + name + '/publish-token',
                        'PUT', {'publish_token': 'owner-check-unused'})[0] == 403
            grant = {'user_id': sessions[other]['user_id'], 'access_level': 'view'}
            assert call(username, path, 'PUT', {'grants': [grant], 'open_visibility': False})[0] == 200
            code, catalog = call(other, 'catalog/position-strategies')
            entry = next(entry for entry in catalog if entry['strategy_name'] == name)
            assert code == 200 and entry['can_publish'] is False
            assert call(other, path, 'PUT', {'grants': [], 'open_visibility': False})[0] == 403
            grant['access_level'] = 'configure'
            assert call(username, path, 'PUT', {'grants': [grant], 'open_visibility': False})[0] == 200
            assert call(other, path, 'PUT', {'grants': [], 'open_visibility': False})[0] == 403
            assert call(username, path, 'PUT', {'grants': [], 'open_visibility': False})[0] == 200
            code, saved = call(username, 'catalog/position-strategies', 'POST',
                               {'strategy_name': name, 'targets': {}})
            assert code == 200 and saved['publishes'] == []
            results[username + '_creator_and_delegation_checks'] = True

        # Unknown order strategy rejects before INSERT; test both permission
        # gates without creating a binding or publishing anything to Exec.
        assert sql("SELECT count(*) FROM cta_order_strategies WHERE strategy_name='" + prefix + "missing';", env) == '0'
        binding = {'binding_name': prefix + 'binding', 'position_strategy_name': prefix + 'shaokai',
                   'order_strategy_name': prefix + 'missing', 'shares': 1}
        code, error = call('shaokai', 'catalog/accounts/binance_exec_trade01/bindings', 'POST', binding)
        assert code == 400 and 'order strategy is unknown' in error['error'], (code, error)
        results['owner_passes_strategy_binding_gate'] = True
        assert call('shaokai', 'catalog/accounts/binance_exec_trade03/bindings', 'POST', binding)[0] == 403
        results['view_only_account_binding_denied'] = True
        assert sql("SELECT count(*) FROM cta_account_strategy_bindings WHERE binding_name='" + prefix + "binding';", env) == '0'

    query = urllib.parse.urlencode({'startMs': 1791849600000, 'endMs': 1791936000000,
                                    'sourceIds': 'binance_exec_trade01', 'maxPoints': 3500,
                                    'includeTheoretical': 'false'})
    code, timeline = call('admin', 'timeline?' + query)
    assert code == 200
    results['fixed_factual_timeline_sha256'] = hashlib.sha256(json.dumps(timeline, sort_keys=True).encode()).hexdigest()
    if phase == 'after':
        code, timeline = call('admin', 'timeline?' + urllib.parse.urlencode(legacy_baseline['query']))
        assert code == 200
        financial = {key: timeline['report'][key] for key in legacy_baseline['financial_keys']}
        results['prior_30d_factual_financial_sha256'] = hashlib.sha256(json.dumps(financial, sort_keys=True).encode()).hexdigest()
        assert results['prior_30d_factual_financial_sha256'] == legacy_baseline['sha256']
        results['prior_30d_factual_results_identical'] = True
    results['roles'] = sql("SELECT username || '|' || role FROM cta_users WHERE username IN ('shaokai','dzy') ORDER BY username;", env).splitlines()
    results['health_http_status'] = call('admin', 'health')[0]
finally:
    for name in reversed(created_names):
        status, _ = call('admin', 'catalog/position-strategies/' + urllib.parse.quote(name), 'DELETE')
        assert status in (204, 404)
    for session in sessions.values():
        sql('DELETE FROM cta_auth_sessions WHERE session_id=' + str(session['session_id']) + ';', env)
    results['temporary_sessions_and_strategies_removed'] = True
    print(json.dumps(results, ensure_ascii=False), flush=True)
