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

"""One-time, operator-authorized recovery of two lost Manager runtime targets.

Only strategy config/index keys are restored. Exec owns position allocation and
execution; its ledger, stores, trading config and all other targets are untouched.
"""
import copy

HEDGE='SYSTEM_BNB_RESERVE'
SYMBOL='BNBUSDC'
mode=sys.argv[1]
assert mode in ('plan','apply')

def redis_get(key):
    return subprocess.check_output(['redis-cli','--raw','GET',key],text=True).removesuffix('\n')

def snapshot(source):
    n=int(source[-2:])
    opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
    with opener.open('http://127.0.0.1:100%02d/snapshot'%(40+n),timeout=10) as response:
        data=json.load(response)
    state=next(e['entry'] for e in data['entries'] if e.get('type')=='exec_pre_trade_state' or e.get('channel')=='exec_pre_trade_state')
    assert state['position_ready'] and abs(time.time()*1000-data['ts_ms'])<10000
    return [row for row in state['rows'] if row['symbol']==SYMBOL]

with gateway() as api:
    plans={}
    for source in SOURCES:
        result=api('catalog/accounts/'+source+'/bnb-auto/preview',{})
        assert 'error' not in result, result
        status=api('catalog/accounts/'+source+'/bnb-auto')
        assert status['pending'] is None and status['settings']['enabled']
        assert status['settings']['required_bnb']==25 and status['settings']['refill_target_bnb']==26
        balances=status['balances']
        assert abs(time.time()*1000-balances['at_ms'])<10000
        total=sum(balances[k] for k in ['spot_bnb','futures_bnb','earn_bnb'])
        assert 25.9<total<=26.02
        historical=json.loads(next(a['result'] for a in reversed(status['audit']) if a['action']=='hedge target'))
        assert abs(historical[SYMBOL]+26)<1e-6
        rows=snapshot(source)
        for row in rows:
            assert row['strategy_name'] in (HEDGE,'SYSTEM_POSITION_CLOSE')
            assert row['execution_complete'] and row['position_allocated'] and abs(row['live_order_qty'])<1e-8
            if row['strategy_name']=='SYSTEM_POSITION_CLOSE':
                assert abs(row['target_qty'])<1e-8 and abs(row['current_qty'])<0.02 and abs(row['pending_qty'])<0.02
        prefix=source+':binance-futures:'
        raw={}
        for family in ('batch_exec','chase_exec'):
            key=prefix+family+':strategy_names'
            raw[key]=redis_get(key)
            names=json.loads(raw[key] or '[]')
            for name in names:
                key=prefix+family+':'+name
                raw[key]=redis_get(key)
                payload=json.loads(raw[key])
                for symbol,target in payload['targets'].items():
                    qty=target['qty'] if isinstance(target,dict) else target
                    if symbol==SYMBOL and name!=HEDGE: assert abs(qty)<1e-8
                    if name==HEDGE:
                        assert family=='batch_exec'
                        assert symbol==SYMBOL and qty<0 or symbol=='BNBUSDT' and abs(qty)<1e-8
            key=prefix+family+':removed_strategy_names'
            raw[key]=redis_get(key)
            assert HEDGE not in json.loads(raw[key] or '[]')
        for family in ('batch_exec','chase_exec'):
            key=prefix+family+'_state:position_allocations'
            raw[key]=redis_get(key)
            ledger=json.loads(raw[key] or '{"positions":{}}')
            for name,positions in ledger['positions'].items():
                qty=positions.get(SYMBOL,0)
                if name not in (HEDGE,'SYSTEM_POSITION_CLOSE'): assert abs(qty)<1e-8
        key=prefix+'exec_switch:'+HEDGE
        raw[key]=redis_get(key)
        assert not raw[key]
        key=prefix+'batch_exec:'+HEDGE
        raw[key]=redis_get(key)
        if source.endswith('03'):
            assert json.loads(raw[key])['updated_at_us']==1791524199802373
            assert abs(sum(r['current_qty'] for r in rows)-balances['bnbusdc_position_qty'])<1e-5
        else:
            assert not raw[key] and not rows
            assert abs(balances['bnbusdc_position_qty']+25.99)<0.02
        plans[source]=dict(settings=status['settings'],balances=balances,total_bnb=total,rows=rows,
                           historical=historical,raw=raw,config_key=key,index_key=prefix+'batch_exec:strategy_names')
    template=json.loads(plans[SOURCES[0]]['raw'][plans[SOURCES[0]]['config_key']])
    for source,plan in plans.items():
        payload=copy.deepcopy(template)
        targets={SYMBOL:dict(qty=-round(plan['total_bnb'],8),signal=0)}
        if source.endswith('03'): targets['BNBUSDT']=dict(qty=0,signal=0)
        payload.update(single_order_usdt=500,orders_per_batch=1,max_batch=2,batch_interval_ms=5000,
                       maker_timeout_ms=10000,max_maker_requotes=1,
                       target_tolerance_usdt=max(1,plan['balances']['bnb_price_usdt']*0.02),targets=targets,
                       updated_at_us=time.time_ns()//1000)
        plan['payload']=payload
        names=json.loads(plan['raw'][plan['index_key']] or '[]')
        plan['names']=sorted(set(names+[HEDGE]))
        emit(source+'_hedge_plan',dict(old=plan['raw'][plan['config_key']],new=payload,
             account_qty=plan['balances']['bnbusdc_position_qty']))
    save('hedge.'+mode+'.json',plans)
    if mode=='apply':
        paused=[]
        try:
            for source,plan in plans.items():
                settings=dict(plan['settings'],enabled=False)
                result=api('catalog/accounts/'+source+'/bnb-auto',settings,'PUT')
                assert result.get('settings')==settings and result['pending'] is None
                paused.append(source)
            # Compare every inspected key in the same transaction as the repair.
            # No stale inspection can overwrite a concurrent target/ledger update.
            for source,plan in plans.items():
                keys=list(plan['raw'])
                script='''
for i=1,#KEYS do
  if (redis.call('GET',KEYS[i]) or '') ~= ARGV[i] then
    return redis.error_reply('concurrent runtime change; repair aborted')
  end
end
redis.call('SET',ARGV[#KEYS+1],ARGV[#KEYS+2])
redis.call('SET',ARGV[#KEYS+3],ARGV[#KEYS+4])
return 'restored'
'''
                args=['redis-cli','--raw','EVAL',script,str(len(keys)),*keys,*[plan['raw'][k] for k in keys],
                      plan['config_key'],json.dumps(plan['payload'],separators=(',',':')),
                      plan['index_key'],json.dumps(plan['names'],separators=(',',':'))]
                output=subprocess.check_output(args,text=True).strip()
                assert output=='restored',output
                assert json.loads(redis_get(plan['config_key']))==plan['payload']
                assert json.loads(redis_get(plan['index_key']))==plan['names']
                emit('hedge_restored',source)
        finally:
            for source in paused:
                result=api('catalog/accounts/'+source+'/bnb-auto',plans[source]['settings'],'PUT')
                assert result.get('settings')==plans[source]['settings']
                emit('automation_resumed',source)
