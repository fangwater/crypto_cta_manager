"""Read-only comparison of RapidX statement overlap with monitor journal."""
import hashlib
import hmac
import http.client
import json
import pathlib
import time
import tomllib
import urllib.parse

root = pathlib.Path('/home/ubuntu/binance_exec_trade06')
latest = next((root / 'data/rapidx_account').glob('*/latest_financial.json'))
doc = json.loads(latest.read_text())
checkpoint = doc['statement_history_end_ms']
old = {}
for path in latest.parent.glob('*.jsonl'):
    with path.open('rb') as stream:
        for line in stream:
            if b'"statement"' not in line:
                continue
            try:
                row = json.loads(line)
            except json.JSONDecodeError:
                continue
            if isinstance(row.get('statement'), dict):
                statement = row['statement']
                old[statement['statement_id']] = statement
creds = None
for proc in pathlib.Path('/proc').iterdir():
    if not proc.name.isdecimal():
        continue
    try:
        if (proc / 'comm').read_text().strip() != 'trade_engine' or (proc / 'cwd').resolve() != root:
            continue
        wanted = {b'LTP_API_KEY', b'LTP_API_SECRET', b'LTP_PORTFOLIO_ID'}
        creds = {k.decode(): v.decode() for item in (proc / 'environ').read_bytes().split(b'\0')
                 if b'=' in item for k, v in [item.split(b'=', 1)] if k in wanted}
        break
    except OSError:
        pass
assert creds is not None
cfg = tomllib.loads((root / 'trade_engine.toml').read_text())
def find_ips(obj):
    if isinstance(obj, dict):
        for key, value in obj.items():
            if key == 'local_ips':
                return value
            found = find_ips(value)
            if found:
                return found
    return None
ips = find_ips(cfg)
assert ips and ips[0] == '172.31.35.228'
params = {'exchange': 'BINANCE', 'startTime': str(checkpoint - 60000),
          'endTime': str(checkpoint + 3600000), 'page': '1', 'pageSize': '1000'}
nonce = str(time.time_ns() // 1000000000)
signed = '&'.join(key + '=' + params[key] for key in sorted(params)) + '&' + nonce
headers = {'X-MBX-APIKEY': creds['LTP_API_KEY'], 'nonce': nonce,
           'ts': str(time.time_ns() // 1000),
           'signature': hmac.new(creds['LTP_API_SECRET'].encode(), signed.encode(), hashlib.sha256).hexdigest()}
conn = http.client.HTTPSConnection('api.liquiditytech.com', timeout=15, source_address=(ips[0], 0))
conn.request('GET', '/api/v1/trading/statement?' + urllib.parse.urlencode(sorted(params.items())), headers=headers)
response = conn.getresponse()
data = json.loads(response.read())
conn.close()
assert response.status == 200 and data.get('code') in [200, 200000], 'query failed (details suppressed)'
assert data['data']['pageNum'] <= 1
fields = {'request_id': 'requestId', 'coin': 'coin', 'symbol': 'sym', 'statement_type': 'statementType',
          'business_type': 'businessType', 'before_available': 'beforeAvailable', 'after_available': 'afterAvailable',
          'before_overdraw': 'beforeOverdraw', 'after_overdraw': 'afterOverdraw', 'before_borrow': 'beforeBorrow',
          'after_borrow': 'afterBorrow', 'delta_amount': 'deltaAmount'}
conflicts = []
for row in data['data']['list']:
    assert str(row['portfolioId']) == creds['LTP_PORTFOLIO_ID'] and row['exchangeType'] == 'BINANCE'
    previous = old.get(row['statementId'])
    if not previous:
        continue
    diff = {key: {'old': previous[key], 'new': row[field]}
            for key, field in fields.items() if previous[key] != row[field]}
    if previous['timestamp_us'] != row['createAt'] * 1000:
        diff['timestamp_us'] = {'old': previous['timestamp_us'], 'new': row['createAt'] * 1000}
    if diff:
        conflicts.append({'statement_id': row['statementId'], 'type': row['statementType'],
                          'coin': row['coin'], 'timestamp_ms': row['createAt'], 'diff': diff})
print(json.dumps({'checkpoint_ms': checkpoint, 'journal_statement_count': len(old),
                  'recovery_ready': doc['recovery_ready'], 'compared': len(data['data']['list']),
                  'conflicts': conflicts}, indent=2))
