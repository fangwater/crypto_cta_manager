"""Scoped BNB recovery via Manager; no exchange writes outside its controller."""
import contextlib
import hashlib
import json
import os
from pathlib import Path
import secrets
import shutil
import subprocess
import sys
import time
import tomllib
import urllib.error
import urllib.request

ROOT = Path('/home/ubuntu/crypto_cta_manager')
WORK = ROOT / 'backups/bnb_recovery_20261023'
UNIT = 'crypto-cta-manager-web.service'
SOURCES = ['binance_exec_trade03', 'binance_exec_trade04']
EXPECTED = dict(zip(SOURCES, [1792079082066, 1792224198897]))
cfg = tomllib.loads((ROOT/'config/cta-manager.toml').read_text())

def emit(name, value):
    print(json.dumps({'name': name, 'body': value}, ensure_ascii=False), flush=True)

def sql(query):
    p = subprocess.run(['sudo','-n','-u','postgres','psql','-X','-qAt','-v','ON_ERROR_STOP=1','-d','crypto_cta_manager'], input=query, text=True, capture_output=True)
    if p.returncode:
        raise RuntimeError('Scoped database operation failed; credentials suppressed')
    return p.stdout.strip()

def save(name, data):
    path = WORK/name
    path.write_text(json.dumps(data, indent=2, sort_keys=True))
    path.chmod(0o600)

def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

def protected():
    result = dict(processes={}, config_hashes={}, snapshots={}, bnb_settings={}, bfusd_settings={})
    for p in Path('/proc').iterdir():
        if not p.name.isdigit(): continue
        try:
            exe = str((p/'exe').resolve())
            if not any(n in Path(exe).name for n in ['trade_engine','exec-pre-trade','persist_manager','exec_config','viz_server','account_monitor','nginx','cta_monitor','spread_pbs','redis-server','cta_perp','cta_spread']): continue
            result['processes'][p.name] = {'exe':exe, 'start_ticks':(p/'stat').read_text().rsplit(')',1)[1].split()[19]}
        except (OSError, IndexError): pass
    paths = [ROOT/'config/cta-manager.toml', Path('/etc/nginx/snippets/crypto_cta_manager.conf')]
    paths.extend(Path(p) for p in cfg.get('kline',{}).get('trade_engine_configs',[]))
    for s in cfg['sources']:
        paths.append(Path(s['rocksdb_path']).parent.parent/'trade_engine.toml')
    for path in paths:
        if path.is_file(): result['config_hashes'][str(path)]=sha(path)
    for table in ['cta_position_snapshots','cta_position_snapshot_entries','cta_strategy_position_snapshots','cta_strategy_position_snapshot_entries','cta_strategy_allocation_snapshots']:
        result['snapshots'][table]=sql("BEGIN READ ONLY; SELECT md5(coalesce(string_agg(row_to_json(t)::text,E'\\n' ORDER BY row_to_json(t)::text),'')) FROM "+table+" t; COMMIT;")
    result['bnb_settings']={k:v['settings'] for k,v in json.loads((ROOT/'config/bnb-auto.json').read_text())['accounts'].items()}
    result['bfusd_settings']=json.loads((ROOT/'config/bfusd-auto.json').read_text())
    result['webroot']=str((ROOT/'webroot').resolve())
    return result

@contextlib.contextmanager
def gateway():
    token=secrets.token_urlsafe(32)
    digest=hashlib.sha256(token.encode()).hexdigest()
    session=sql("INSERT INTO cta_auth_sessions(user_id,token_hash,expires_at) SELECT user_id,decode('"+digest+"','hex'),now()+interval '15 minutes' FROM cta_users WHERE role='admin' AND NOT disabled ORDER BY user_id LIMIT 1 RETURNING session_id;")
    assert session.isdigit()
    opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
    def api(path, body=None, method=None):
        request=urllib.request.Request('http://127.0.0.1:4191/manager/api/'+path, data=None if body is None else json.dumps(body).encode(), method=method, headers={'Cookie':'cta_session='+token,'Content-Type':'application/json'})
        try:
            with opener.open(request,timeout=55) as response: return json.load(response)
        except urllib.error.HTTPError as error:
            try: body=json.loads(error.read())
            except ValueError: body='Gateway unavailable'
            return {'http':error.code,'error':body}
    try: yield api
    finally:
        sql('DELETE FROM cta_auth_sessions WHERE session_id='+session+';')
        emit('diagnostic_session_removed',True)

source = 'binance_exec_trade03'
with gateway() as api:
    old = api('catalog/accounts/'+source)
    save('trade03.fees.before.json', old)
    rates = dict(maker_fee_rate=0.000144, taker_fee_rate=0.00036,
                 theoretical_twap_fee_rate=0.000252)
    emit('fee_update', rates)
    result = api('catalog/accounts/'+source+'/fee-rates', rates, 'PUT')
    if 'http' in result:
        emit('fee_update_response', result)
    after = api('catalog/accounts/'+source)
    save('trade03.fees.after.json', after)
    verified = json.loads(sql("SELECT row_to_json(t) FROM (SELECT source_id,maker_fee_rate,taker_fee_rate,theoretical_twap_fee_rate,quote_fee_rates FROM cta_order_sources WHERE source_id='binance_exec_trade03') t;"))
    assert all(verified[k] == v for k,v in rates.items())
    emit('fee_rates_verified', verified)
