"""Read-only probes; credentials remain in memory on the account host."""
import hashlib
import hmac
import http.client
import ipaddress
import json
import pathlib
import subprocess
import time
import tomllib
import urllib.parse

root = pathlib.Path('/home/ubuntu/crypto_cta_manager')
stage = root / 'backups/funding_estimate_20261015'
config = tomllib.loads((root / 'config/cta-manager.toml').read_text())

def get(host, path, ip, headers=None):
    conn = http.client.HTTPSConnection(host, timeout=25, source_address=(ip, 0))
    conn.request('GET', path, headers=headers or {})
    response = conn.getresponse()
    body = response.read()
    conn.close()
    assert response.status == 200, ('read failed', host, response.status)
    return body

def verify(ip, expected):
    assert ip not in ['0.0.0.0', '::']
    assert get('api.ipify.org', '/', ip).decode().strip() == expected

# Public mark-price candles use the already configured dedicated Kline egress.
kline = config['kline']
assert kline['enabled'] and kline['public_ip'] not in kline['forbidden_public_ips']
assert len(kline['trade_engine_configs']) >= 24
checked = 0
def check_ip_fields(value):
    if isinstance(value, dict):
        for key, val in value.items():
            if 'ip' in key.lower():
                for candidate in val if isinstance(val, list) else [val]:
                    if not isinstance(candidate, str):
                        continue
                    try:
                        addr = ipaddress.ip_address(candidate)
                    except ValueError:
                        continue
                    if addr.is_unspecified:
                        route = json.loads(subprocess.check_output(['ip', '-j', 'route', 'get', '1.1.1.1'], text=True))
                        candidate = route[0]['prefsrc']
                    assert candidate != kline['local_ip'] and candidate != kline['public_ip']
            elif isinstance(val, dict):
                check_ip_fields(val)
for path in kline['trade_engine_configs']:
    check_ip_fields(tomllib.loads(pathlib.Path(path).read_text()))
    checked += 1
verify(kline['local_ip'], kline['public_ip'])
reports = json.loads((stage / 'comparison.json').read_text())['reports']
samples = {}
for report in reports:
    if report['allocation']:
        continue
    candidates = sorted(report['comparisons'], key=lambda x: abs(x['error']), reverse=True)
    for row in candidates[:4]:
        if row.get('observed', True) and row['actual'] and row['quantity'] and row['rate']:
            samples[(row['symbol'], row['ts_us'] // 60_000_000 * 60_000)] = row
marks = []
for (symbol, ts), row in samples.items():
    params = {'symbol': symbol, 'interval': '1m', 'startTime': ts - 120_000,
              'endTime': ts + 300_000, 'limit': 10}
    candles = json.loads(get('fapi.binance.com', '/fapi/v1/markPriceKlines?' + urllib.parse.urlencode(params), kline['local_ip']))
    assert isinstance(candles, list)
    marks.append({'symbol': symbol, 'settlement_ms': ts, 'funding_mark': row['mark_price'],
                  'implied_mark': -row['actual'] / row['quantity'] / row['rate'], 'candles': candles})
    time.sleep(0.2)

# Factual statements retain the explicitly authorized fixed RapidX trading IP.
statement_probes = []
queries = [('06', 'ADA', 1791964800000), ('06', 'DOT', 1791907200000),
           ('05', 'AAVE', 1791993600000), ('06', 'AAVE', 1791993600000), ('07', 'AAVE', 1791993600000)]
for number, asset, ts in queries:
    source = 'binance_exec_trade' + number
    assert source in config['funding']['rapidx_trading_ip_sources']
    account_root = pathlib.Path('/home/ubuntu') / source
    ips = tomllib.loads((account_root / 'trade_engine.toml').read_text())['local_ips']
    ip = ips[0]
    assert ip == config['funding']['local_ip']
    matches = []
    for proc in pathlib.Path('/proc').iterdir():
        if not proc.name.isdecimal():
            continue
        try:
            if (proc / 'comm').read_text().strip() == 'trade_engine' and (proc / 'cwd').resolve() == account_root:
                matches.append(proc)
        except OSError:
            pass
    assert len(matches) == 1
    wanted = {b'LTP_API_KEY', b'LTP_API_SECRET', b'LTP_PORTFOLIO_ID', b'LTP_REST_URL'}
    env = {k.decode(): v.decode() for item in (matches[0] / 'environ').read_bytes().split(b'\0')
           if b'=' in item for k, v in [item.split(b'=', 1)] if k in wanted}
    assert all(env.get(x) for x in ['LTP_API_KEY', 'LTP_API_SECRET', 'LTP_PORTFOLIO_ID'])
    assert env.get('LTP_REST_URL', 'https://api.liquiditytech.com').rstrip('/') == 'https://api.liquiditytech.com'
    verify(ip, config['funding']['public_ip'])
    sym = 'BINANCE_PERP_' + asset + '_USDT'
    params = {'exchange': 'BINANCE', 'sym': sym, 'startTime': str(ts - 60_000),
              'endTime': str(ts + 600_000), 'statementType': 'FUNDING_FEE', 'page': '1', 'pageSize': '1000'}
    nonce = str(time.time_ns() // 1_000_000_000)
    signed = '&'.join(k + '=' + params[k] for k in sorted(params)) + '&' + nonce
    headers = {'X-MBX-APIKEY': env['LTP_API_KEY'], 'nonce': nonce, 'ts': str(time.time_ns() // 1000),
               'signature': hmac.new(env['LTP_API_SECRET'].encode(), signed.encode(), hashlib.sha256).hexdigest()}
    body = json.loads(get('api.liquiditytech.com', '/api/v1/trading/statement?' + urllib.parse.urlencode(sorted(params.items())), ip, headers))
    assert body.get('code') in [200, 200000]
    rows = body['data']['list']
    assert len(rows) == int(body['data']['totalSize']) == 1
    assert all(str(r['portfolioId']) == env['LTP_PORTFOLIO_ID'] and r['exchangeType'] == 'BINANCE'
               and r['sym'] == sym and r['statementType'] == 'FUNDING_FEE' for r in rows)
    statement_probes.append({'source_id': source, 'fields': sorted(rows[0]), 'statements': [
        {k: r.get(k) for k in ['statementId', 'coin', 'sym', 'statementType', 'businessType', 'deltaAmount', 'createAt']}
        for r in rows]})

out = {'checked_trading_configs': checked, 'marks': marks, 'statements': statement_probes}
(stage / 'rapidx_probe.json').write_text(json.dumps(out, indent=2) + '\n')
print(json.dumps({'mark_queries': len(marks), 'statement_queries': len(statement_probes), 'checked_trading_configs': checked}))
