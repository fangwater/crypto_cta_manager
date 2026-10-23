"""Read-only BNB recovery evidence; credentials never leave process memory."""
import hashlib
import hmac
import http.client
import json
from decimal import Decimal
from pathlib import Path
import time
import tomllib
import urllib.parse

ROOT = Path('/home/ubuntu/crypto_cta_manager')
cfg = tomllib.loads((ROOT / 'config/cta-manager.toml').read_text())
disk = json.loads((ROOT / 'config/bnb-auto.json').read_text())

def emit(name, value):
    print(json.dumps({'name': name, 'body': value}, ensure_ascii=False), flush=True)

for source in cfg['sources']:
    source_id = source['id']
    if source_id not in ('binance_exec_trade03', 'binance_exec_trade04'):
        continue
    root = Path(source['rocksdb_path']).parent.parent
    ips = tomllib.loads((root / 'trade_engine.toml').read_text())['local_ips']
    ip = ips[0]
    assert ip == '172.31.35.228', 'Trading binding changed; review required'
    env = {}
    for line in Path(source.get('env_path', root / 'env.sh')).read_text().splitlines():
        line = line.strip().removeprefix('export ').strip()
        if line.startswith('#') or '=' not in line:
            continue
        key, value = line.split('=', 1)
        env[key.strip()] = value.strip().strip('\"\'')
    assert env['BINANCE_ACCOUNT_MODE'].lower() in ('standard', 'std')

    def get(path, params=None, futures=False):
        base = env.get('BINANCE_FAPI_URL', 'https://fapi.binance.com') if futures else env.get('BINANCE_SAPI_URL', env.get('BINANCE_API_URL', 'https://api.binance.com'))
        host = urllib.parse.urlsplit(base)
        assert host.scheme == 'https' and host.hostname in ('api.binance.com', 'fapi.binance.com')
        query = urllib.parse.urlencode(dict(params or {}, recvWindow=5000, timestamp=int(time.time()*1000)))
        signature = hmac.new(env['BINANCE_API_SECRET'].encode(), query.encode(), hashlib.sha256).hexdigest()
        conn = http.client.HTTPSConnection(host.hostname, timeout=15, source_address=(ip, 0))
        try:
            conn.request('GET', path + '?' + query + '&signature=' + signature, headers={'X-MBX-APIKEY': env['BINANCE_API_KEY']})
            response = conn.getresponse()
            value = json.loads(response.read())
            if response.status != 200:
                raise RuntimeError(f'Binance read {path} HTTP {response.status} code {value.get("code")}')
            return value
        finally:
            conn.close()

    def pages(path, params):
        rows = []
        for current in range(1, 101):
            value = get(path, dict(params, current=current, size=100))
            rows.extend(value['rows'])
            if len(rows) >= int(value['total']):
                assert len(rows) == int(value['total'])
                return rows
            assert value['rows'], 'Incomplete history'
        raise RuntimeError('Pagination limit exceeded')

    pending = disk['accounts'][source_id]['pending']
    if pending:
        at = pending['at_ms']
        history = pages('/sapi/v1/simple-earn/flexible/history/redemptionRecord',
                        dict(asset='BNB', productId='BNB001', startTime=at-3600000, endTime=int(time.time()*1000)))
        assert all(row['asset']=='BNB' and row['productId']=='BNB001' for row in history)
        assert not any(Decimal(row['amount']) == Decimal(pending['params']['amount']) for row in history), 'Pending amount found in exchange history; review outcome'
        emit(source_id + '_redemptions', {'pending_at_ms': at, 'pending_amount': pending['params']['amount'], 'complete': True, 'rows': history})
    spot = get('/api/v3/account')
    futures = get('/fapi/v2/account', futures=True)
    open_orders = get('/fapi/v1/openOrders', dict(symbol='BNBUSDC'), futures=True)
    emit(source_id + '_bnbusdc_open_orders', {'count':len(open_orders)})
    assert not open_orders, 'BNBUSDC has exchange orders in flight'
    emit(source_id + '_fees', {'fee_tier': futures.get('feeTier'),
         'fee_burn': get('/fapi/v1/feeBurn', futures=True),
         'symbols': [get('/fapi/v1/commissionRate', dict(symbol=symbol), futures=True)
                     for symbol in ('BTCUSDT','BTCUSDC','BNBUSDC')]})
    earn = pages('/sapi/v1/simple-earn/flexible/position', dict(asset='BNB'))
    s = next(v for v in spot['balances'] if v['asset'] == 'BNB')
    f = next(v for v in futures['assets'] if v['asset'] == 'BNB')
    emit(source_id + '_balances', {'at_ms': int(time.time()*1000), 'local_ip': ip,
         'spot_free': s['free'], 'spot_locked': s['locked'], 'futures_bnb': f['walletBalance'],
         'earn': earn, 'total_bnb': float(s['free'])+float(s['locked'])+float(f['walletBalance'])+sum(float(v['totalAmount']) for v in earn),
         'hedge': [{k:v[k] for k in ('symbol','positionAmt')} for v in futures['positions'] if v['symbol']=='BNBUSDC']})
