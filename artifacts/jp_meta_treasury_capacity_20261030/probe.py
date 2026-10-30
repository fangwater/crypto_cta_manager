"""Public read-only egress and exchange-weight evidence, explicitly bound."""
import concurrent.futures
import datetime
import http.client
import json
from pathlib import Path
import tomllib

root = Path('/home/ubuntu/crypto_cta_manager')
cfg = tomllib.loads((root / 'config/cta-manager.toml').read_text())
ips = []
for source in cfg['sources']:
    if source['id'] not in ['binance_exec_trade03', 'binance_exec_trade04']:
        continue
    path = Path(source['rocksdb_path']).parent.parent / 'trade_engine.toml'
    addresses = tomllib.loads(path.read_text())['local_ips']
    print(json.dumps(dict(source=source['id'], local_ips=addresses)), flush=True)
    for ip in addresses:
        if ip not in ips:
            ips.append(ip)
assert ips == ['172.31.35.228', '172.31.35.234'], 'Account IPs changed'

def query(ip):
    result = dict(at=datetime.datetime.now(datetime.timezone.utc).isoformat(), local_ip=ip)
    for name, host, path in [('public_ip', 'api.ipify.org', '/?format=json'),
                             ('usd_m', 'fapi.binance.com', '/fapi/v1/exchangeInfo')]:
        conn = http.client.HTTPSConnection(host, timeout=12, source_address=(ip, 0))
        try:
            conn.request('GET', path)
            response = conn.getresponse()
            body = json.loads(response.read())
            result[name] = dict(http=response.status,
                value=body if name == 'public_ip' else body.get('rateLimits'),
                weight_headers={k: v for k, v in response.getheaders() if 'weight' in k.lower()})
        finally:
            conn.close()
    return result

with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
    for result in pool.map(query, ips):
        print(json.dumps(result), flush=True)
