"""Append to the existing Manager diagnostic helper; emit no credentials."""
import math
import re

opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def upstream():
    settings = cfg['dashboard']['nav_overview']
    token = runtime[settings['token_env'].encode()].decode()
    request = urllib.request.Request(settings['url'], headers={'Authorization': 'Bearer ' + token})
    with opener.open(request, timeout=20) as response:
        return json.load(response)


def verify_rows(report):
    rows = {row['source_id']: row for row in report['accounts']}
    for slug, expected in {
        'gate_fr_arb01': {'SLVON', 'TSLAX'},
        'gate_fr_arb02': {'CRCLX', 'FTM', 'MATIC', 'SLVON', 'TSLAX'},
    }.items():
        row = next(row for key, row in rows.items() if key == slug or key.endswith(':' + slug))
        assert row['collection_status'] == 'live', slug
        assert set(row['position_unpriced_assets']) == expected, slug
        for key in ['long_position', 'short_position', 'net_position', 'long_leverage', 'short_leverage']:
            assert row[key] is not None and math.isfinite(row[key]), (slug, key)
        assert abs(row['net_position'] - (row['long_position'] - row['short_position'])) < 1e-8
        assert abs(row['long_leverage'] - row['long_position'] / row['equity']) < 1e-10
        assert abs(row['short_leverage'] - row['short_position'] / row['equity']) < 1e-10
    return rows


if MODE == 'before':
    emit('protected', protected())
    emit('nav_before', upstream())
    emit('nav_migrations_hashes', {p.name: hashlib.sha256(p.read_bytes()).hexdigest()
                                 for p in Path('/home/ubuntu/crypto_nav_manager/migrations').glob('*.sql')})
    emit('nav_affinity', next(line for line in Path('/proc/' + subprocess.check_output(
        ['systemctl', 'show', 'crypto-nav-manager.service', '-p', 'MainPID', '--value'], text=True).strip()
        + '/status').read_text().splitlines() if line.startswith('Cpus_allowed_list:')))
else:
    current = upstream()
    verify_rows(current)
    emit('nav_after', current)
    with gateway() as get:
        get('health')
        report = get('account-overview')
        verify_rows(report)
        emit('cta_after', report)
        # Historical exposure must not borrow today's incomplete holdings.
        if current['dates']:
            settings = cfg['dashboard']['nav_overview']
            token = runtime[settings['token_env'].encode()].decode()
            request = urllib.request.Request(settings['url'] + '?date=' + current['dates'][0],
                                             headers={'Authorization': 'Bearer ' + token})
            with opener.open(request, timeout=20) as response:
                historical = json.load(response)
            for row in historical['accounts']:
                assert row['long_position'] is None and row['short_position'] is None
                assert not row['position_unpriced_assets']
            emit('historical_exposure_unknown', True)
    for path in ['/nav/', '/nav-api/health', '/manager/']:
        with opener.open('http://127.0.0.1:4191' + path, timeout=20) as response:
            body = response.read()
            emit('gateway', {'path': path, 'status': response.status})
        if path.endswith('/'):
            assets = re.findall(rb'(?:src|href)="([^\"]+\.(?:js|css))"', body)
            assert assets
            for asset in assets:
                with opener.open('http://127.0.0.1:4191' + asset.decode(), timeout=20) as response:
                    content = response.read()
                    emit('asset', {'path': asset.decode(), 'sha256': hashlib.sha256(content).hexdigest()})
    emit('protected', protected())
