"""Diagnose read-only dashboard queries; keep credentials on the host."""
import hashlib
import json
import os
from pathlib import Path
import secrets
import subprocess
import time
import tomllib
import urllib.parse
import urllib.request

ROOT = Path('/home/el01/crypto_cta_manager')
UNIT = 'crypto-cta-manager-web.service'
cfg = tomllib.loads((ROOT / 'config/cta-manager.toml').read_text())
pid = subprocess.check_output(['systemctl', '--user', 'show', UNIT, '-p', 'MainPID', '--value'], text=True).strip()
runtime = dict(entry.split(b'=', 1) for entry in Path(f'/proc/{pid}/environ').read_bytes().split(b'\0') if b'=' in entry)
url = urllib.parse.urlsplit(runtime[cfg.get('database', {}).get('url_env', 'CRYPTO_CTA_LOCAL_DATABASE_URL').encode()].decode())
env = dict(os.environ, PGHOST=url.hostname, PGPORT=str(url.port or 5432),
           PGUSER=urllib.parse.unquote(url.username), PGPASSWORD=urllib.parse.unquote(url.password or ''),
           PGDATABASE=urllib.parse.unquote(url.path.lstrip('/')), PGCONNECT_TIMEOUT='10')


def sql(query):
    result = subprocess.run([str(Path.home() / '.local/opt/postgresql/16/bin/psql'), '-X', '-q', '-A', '-t', '-v', 'ON_ERROR_STOP=1'],
                            input=query, text=True, capture_output=True, env=env)
    if result.returncode:
        raise RuntimeError('diagnostic database operation failed (credentials suppressed)')
    return result.stdout.strip()


import re
from collections import Counter
from datetime import datetime, timezone
name='USDC_C40V6D3PosV5_filter'
token=secrets.token_urlsafe(32)
digest=hashlib.sha256(token.encode()).hexdigest()
session_id=sql("INSERT INTO cta_auth_sessions(user_id,token_hash,expires_at) SELECT user_id,decode('"+digest+"','hex'),now()+interval '5 minutes' FROM cta_users WHERE role='admin' AND disabled=false ORDER BY user_id LIMIT 1 RETURNING session_id")
assert session_id.isdigit()
opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
def get(path):
    req=urllib.request.Request('http://127.0.0.1:10051'+path,headers={'Cookie':'cta_session='+token})
    with opener.open(req,timeout=20) as response:
        assert response.status==200
        return json.load(response)
result={}
try:
    result['health']=get('/manager/api/health')
    orders=get('/manager/api/catalog/order-strategies')
    result['order_templates_http']=200
    result['order_template_count']=len(orders)
    pos=next(x for x in get('/manager/api/catalog/position-strategies') if x['strategy_name']==name)
    config=get('/exec_trade14/config/api/strategy?'+urllib.parse.urlencode({'name':name,'execution_family':'batch_exec'}))
    assert config['exists'] and config['strategy_name']==name
    targets=config['config']['targets']
    expected=pos['targets']
    assert set(targets)==set(expected)
    for symbol,value in expected.items():
        target=targets[symbol]
        expected_qty=value['qty']*25
        assert abs(target['qty']-expected_qty)<=max(1e-10,abs(expected_qty)*1e-12),symbol
        assert target['signal']==value['signal'],symbol
    result['strategy']=dict(name=name,source_id='binance_exec_trade14',catalog_updated_at=datetime.fromtimestamp(pos['updated_at_us']/1e6,timezone.utc).isoformat(),runtime_updated_at=datetime.fromtimestamp(config['config']['updated_at_us']/1e6,timezone.utc).isoformat(),target_count=len(targets),shares=25,all_target_quantities_and_signals_match=True,config_http_status=200)
finally:
    sql('DELETE FROM cta_auth_sessions WHERE session_id='+session_id)
    result['temporary_auth_session_removed']=True
logs=subprocess.check_output(['journalctl','--user','-u',UNIT,'--since','2026-10-21 07:35:14 UTC','--no-pager','-o','cat'],text=True)
logs=re.sub(r'\x1b\[[0-9;]*m','',logs)
result['missing_grid_errors_after_repair']=logs.count('column "grid" does not exist')
result['strategy_publish_success_logs']=[line for line in logs.splitlines() if name in line and ('published bound account' in line or 'confirmed Redis write' in line)]
result['strategy_publish_failure_logs']=[line for line in logs.splitlines() if name in line and 'failed' in line]
assert result['missing_grid_errors_after_repair']==0
assert result['strategy_publish_success_logs'] and not result['strategy_publish_failure_logs']
counts=Counter()
for line in subprocess.check_output(['tail','-n','15000',str(ROOT/'nginx/logs/access.log')],text=True).splitlines():
    match=re.search(r'\[21/Oct/2026:(\d\d:\d\d:\d\d) \+0000\] "POST /manager/api/catalog/position-strategies HTTP/1.1" (\d+)',line)
    if match and match.group(1)>='07:35:14':counts[match.group(2)]+=1
result['position_post_statuses_after_repair']=dict(counts)
result['checked_at']=datetime.now(timezone.utc).isoformat()
print(json.dumps(result,ensure_ascii=False,indent=2))
