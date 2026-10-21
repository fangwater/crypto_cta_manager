import collections
import copy
import datetime
import hashlib
import json
from pathlib import Path
import secrets
import subprocess
import sys
import time
import urllib.request

ROOT = Path('/home/ubuntu/crypto_cta_manager/backups/grid_split_20261021')
MAPPING = {'virtual02': ('binance_exec_trade05', 'lgq_grid_prep'), 'virtual03': ('binance_exec_trade06', 'lgq_grid_spread'), 'virtual04': ('binance_exec_trade07', 'lgq_grid_prep')}

def sql(query):
    p = subprocess.run(['sudo','-n','-u','postgres','psql','-X','-qAt','-v','ON_ERROR_STOP=1','-d','crypto_cta_manager'], input=query, text=True, capture_output=True)
    if p.returncode: raise RuntimeError(p.stderr)
    return p.stdout.strip()

def save(name, data):
    ROOT.mkdir(parents=True, exist_ok=True, mode=0o700)
    p = ROOT / name
    p.write_text(json.dumps(data, indent=2, sort_keys=True))
    p.chmod(0o600)

class Session:
    def __init__(self, condition):
        self.token = secrets.token_urlsafe(32)
        digest = hashlib.sha256(self.token.encode()).hexdigest()
        self.id = sql("INSERT INTO cta_auth_sessions(user_id,token_hash,expires_at) SELECT user_id,decode('"+digest+"','hex'),now()+interval '20 minutes' FROM cta_users WHERE NOT disabled AND "+condition+" ORDER BY user_id LIMIT 1 RETURNING session_id;")
        assert self.id.isdigit()
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    def api(self, path, body=None, method=None):
        request = urllib.request.Request('http://127.0.0.1:4191'+path, data=None if body is None else json.dumps(body).encode(), method=method, headers={'Cookie':'cta_session='+self.token,'Content-Type':'application/json'})
        with self.opener.open(request, timeout=30) as response:
            raw = response.read()
            return json.loads(raw) if raw else None
    def close(self):
        sql('DELETE FROM cta_auth_sessions WHERE session_id='+self.id+';')

def main():
    admin = Session("role='admin'")
    owner = None
    try:
        mode = sys.argv[1] if len(sys.argv)>1 else 'inspect'
        orders = admin.api('/manager/api/catalog/order-strategies')
        virtuals = admin.api('/manager/api/catalog/virtual-accounts')
        positions = admin.api('/manager/api/catalog/position-strategies')
        original = next((o for o in orders if o['strategy_name']=='lgq_grid'), None)
        if original is None:
            original = json.loads((ROOT/'before.json').read_text())['original']
        assert original['owner']=='lgq' and original['order_parameters']['algorithm']=='grid'
        specs = {}
        for name in ['lgq_grid_spread','lgq_grid_prep']:
            params = copy.deepcopy(original['order_parameters'])
            if name=='lgq_grid_prep': params['grid']['futures']=copy.deepcopy(params['grid']['spot'])
            specs[name]={'strategy_name':name,'order_parameters':params}
        for name, spec in specs.items():
            existing = next((o for o in orders if o['strategy_name']==name),None)
            if existing: assert existing['order_parameters']==spec['order_parameters'] and existing['owner']=='lgq', 'Existing template differs'
        references = json.loads(sql("SELECT json_build_object('real',(SELECT coalesce(json_agg(b),'[]') FROM cta_account_strategy_bindings b WHERE order_strategy_name='lgq_grid'),'virtual',(SELECT coalesce(json_agg(b),'[]') FROM cta_virtual_account_bindings b WHERE order_strategy_name='lgq_grid'));"))
        assert all(b['source_id'] in {s for s,n in MAPPING.values()} for b in references['real'])
        assert all(b['virtual_id'] in MAPPING for b in references['virtual'])
        assert not any('lgq_grid' in p.get('symbol_order_strategy_overrides',{}).values() for p in positions)
        for v, (s,name) in MAPPING.items():
            a = next(a for a in virtuals if a['virtual_id']==v)
            assert {f['source_id'] for f in a['followers']}=={s}
            print('PLAN',s,name,len(a['bindings']),json.dumps(specs[name]['order_parameters']['grid']))
            if name=='lgq_grid_prep':
                active = {b['position_strategy_name'] for b in a['bindings'] if b['shares']>0}
                assert all(sym.endswith(('USDT','USDC')) for p in positions if p['strategy_name'] in active for sym in p['targets']), 'Prep account has cash targets'
        record={'at':datetime.datetime.now(datetime.timezone.utc).isoformat(),'original':original,'orders':orders,'virtuals':virtuals,'references':references,'specs':specs}
        save(mode+'_'+str(time.time_ns())+'.json',record)
        if mode=='execute':
            assert not (ROOT/'before.json').exists(), 'Activation already started; inspect before retry'
            save('before.json',record)
            owner=Session("username='lgq'")
            for name,spec in specs.items():
                result=owner.api('/manager/api/catalog/order-strategies',spec,'POST')
                assert result['owner']=='lgq' and result['order_parameters']==spec['order_parameters']
                print('TEMPLATE_SAVED',name,flush=True)
            for v,(s,name) in MAPPING.items():
                old=next(a for a in virtuals if a['virtual_id']==v)
                fresh=next(a for a in admin.api('/manager/api/catalog/virtual-accounts') if a['virtual_id']==v)
                assert fresh['bindings']==old['bindings'] and fresh['updated_at_us']==old['updated_at_us'], 'Concurrent binding change'
                body={'name':old['name'],'bindings':copy.deepcopy(old['bindings'])}
                for b in body['bindings']: b['order_strategy_name']=name
                result=admin.api('/manager/api/catalog/virtual-accounts/'+v,body,'PUT')
                save('applied_'+v+'.json',result)
                print('BOUND',s,name,len(result['bindings']),flush=True)
        if mode in ['execute','verify']:
            original_virtuals=json.loads((ROOT/'before.json').read_text())['virtuals']
            checks={}
            for v,(s,name) in MAPPING.items():
                studio=admin.api('/manager/api/catalog/accounts/'+s)
                checks[s]={'pending':studio['pending_publishes'],'templates':dict(collections.Counter(b['order_strategy_name'] for b in studio['bindings']))}
                print('STUDIO',s,json.dumps(checks[s]))
                if mode=='verify':
                    assert not studio['pending_publishes']
                    assert all(b['order_strategy_name']==name for b in studio['bindings'])
                    old=next(a for a in original_virtuals if a['virtual_id']==v)
                    fields=lambda bs:sorted((b['binding_name'],b['position_strategy_name'],b['shares']) for b in bs)
                    assert fields(old['bindings'])==fields(studio['bindings'])
                    for b in studio['bindings']:
                        for market in ['binance-futures','binance-margin']:
                            raw=subprocess.check_output(['redis-cli','--raw','GET',s+':'+market+':batch_exec:'+b['binding_name']],text=True)
                            val=json.loads(raw)
                            assert val['algorithm']=='grid' and val['grid']==specs[name]['order_parameters']['grid']
            save('verification_'+mode+'.json',checks)
            if mode=='verify' and any(o['strategy_name']=='lgq_grid' for o in orders):
                assert not references['real'] and not references['virtual']
                admin.api('/manager/api/catalog/order-strategies/lgq_grid',method='DELETE')
                print('UNBOUND_ORIGINAL_REMOVED')
            final=admin.api('/manager/api/catalog/order-strategies')
            save('final_templates.json',[o for o in final if o['strategy_name'].startswith('lgq_grid')])
    finally:
        if owner: owner.close()
        admin.close()
        print('TEMP_SESSIONS_REMOVED')

if __name__=='__main__': main()
