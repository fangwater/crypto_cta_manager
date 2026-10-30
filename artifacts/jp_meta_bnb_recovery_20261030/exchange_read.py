"""Bound read-only exchange evidence; never prints credentials or signed URLs."""
import hashlib
import hmac
import http.client
import json
from pathlib import Path
import time
import tomllib
import urllib.parse

ROOT = Path('/home/ubuntu/crypto_cta_manager')
cfg = tomllib.loads((ROOT / 'config/cta-manager.toml').read_text())
old = json.loads((ROOT / 'backups/bnb-self-heal-20261030/bnb-auto.json.before').read_text())
for source in cfg['sources']:
    if source['id'] not in ['binance_exec_trade03', 'binance_exec_trade04']:
        continue
    root = Path(source['rocksdb_path']).parent.parent
    ip = tomllib.loads((root / 'trade_engine.toml').read_text())['local_ips'][0]
    assert ip == '172.31.35.228', 'Trading IP changed; review required'
    env = {}
    for line in Path(source.get('env_path', root / 'env.sh')).read_text().splitlines():
        line = line.strip().removeprefix('export ').strip()
        if line.startswith('#') or '=' not in line:
            continue
        key, value = line.split('=', 1)
        env[key.strip()] = value.strip().strip('\"\'')

    def get(path, params=None, futures=False):
        base = env.get('BINANCE_FAPI_URL', 'https://fapi.binance.com') if futures else env.get(
            'BINANCE_SAPI_URL', env.get('BINANCE_API_URL', 'https://api.binance.com'))
        host = urllib.parse.urlsplit(base)
        assert host.scheme == 'https' and host.hostname in ['api.binance.com', 'fapi.binance.com']
        query = urllib.parse.urlencode(dict(params or {}, recvWindow=5000, timestamp=int(time.time() * 1000)))
        signature = hmac.new(env['BINANCE_API_SECRET'].encode(), query.encode(), hashlib.sha256).hexdigest()
        conn = http.client.HTTPSConnection(host.hostname, timeout=15, source_address=(ip, 0))
        try:
            conn.request('GET', path + '?' + query + '&signature=' + signature,
                         headers={'X-MBX-APIKEY': env['BINANCE_API_KEY']})
            response = conn.getresponse()
            value = json.loads(response.read())
            assert response.status == 200, f'Binance read {path} HTTP {response.status}'
            return value
        finally:
            conn.close()

    def pages(path, params):
        rows = []
        for page in range(1, 101):
            value = get(path, dict(params, current=page, size=100))
            rows.extend(value['rows'])
            if len(rows) >= int(value['total']):
                assert len(rows) == int(value['total'])
                return rows
            assert value['rows'], 'Incomplete history'
        raise RuntimeError('Pagination limit exceeded')

    pending = old['accounts'][source['id']]['pending']
    if pending:
        assert pending['at_ms'] == 1793128208622
        history = pages('/sapi/v1/asset/transfer', dict(type=pending['params']['type'],
            startTime=pending['at_ms'] - 60000, endTime=pending['at_ms'] + 300000))
        rows = [r for r in history if str(r['tranId']) == str(pending['response']['tranId'])]
        assert len(rows) == 1 and rows[0]['status'] == 'CONFIRMED'
        assert all(rows[0][k] == pending['params'][k] for k in ['asset', 'amount', 'type'])
        print(json.dumps({'source': source['id'], 'confirmed_original_transfer': rows[0]}), flush=True)
    spot = get('/api/v3/account')
    futures = get('/fapi/v2/account', futures=True)
    earn = pages('/sapi/v1/simple-earn/flexible/position', dict(asset='BNB'))
    spot_bnb = next(v for v in spot['balances'] if v['asset'] == 'BNB')
    futures_bnb = next(v for v in futures['assets'] if v['asset'] == 'BNB')
    balances = dict(at_ms=int(time.time() * 1000), local_ip=ip,
        spot_bnb=float(spot_bnb['free']) + float(spot_bnb['locked']),
        spot_free_bnb=float(spot_bnb['free']), futures_bnb=float(futures_bnb['walletBalance']),
        earn_bnb=sum(float(r['totalAmount']) for r in earn),
        hedge=[{k: v[k] for k in ['symbol', 'positionAmt']} for v in futures['positions'] if v['symbol'] == 'BNBUSDC'],
        open_orders_count=len(get('/fapi/v1/openOrders', dict(symbol='BNBUSDC'), futures=True)),
        fee_burn=get('/fapi/v1/feeBurn', futures=True))
    balances['total_bnb'] = balances['spot_bnb'] + balances['futures_bnb'] + balances['earn_bnb']
    print(json.dumps({'source': source['id'], 'balances': balances}), flush=True)
