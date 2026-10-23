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
