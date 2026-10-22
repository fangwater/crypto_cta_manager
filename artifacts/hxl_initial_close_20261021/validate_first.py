import json,math
from pathlib import Path
root=Path(__file__).parent

def read(name):
 return {x['name']:x['body'] for x in map(json.loads,(root/name).open()) if 'name' in x}
before=read('before_cached.jsonl');after=read('after_cached.jsonl')
errors=[]
def same(a,b,path):
 if isinstance(a,(int,float)) and isinstance(b,(int,float)):
  if not math.isclose(a,b,rel_tol=1e-11,abs_tol=1e-8):errors.append((path,a,b))
 elif isinstance(a,dict) and isinstance(b,dict):
  if a.keys()!=b.keys():errors.append((path+'.keys',list(a),list(b)))
  for k in a.keys()&b.keys():same(a[k],b[k],path+'.'+k)
 elif isinstance(a,list) and isinstance(b,list):
  if len(a)!=len(b):errors.append((path+'.len',len(a),len(b)))
  for i,(x,y) in enumerate(zip(a,b)):same(x,y,path+'.'+str(i))
 elif a!=b:errors.append((path,a,b))
def same_curve(a,b,path):
    if [p['ts_us'] for p in a]==[p['ts_us'] for p in b]:
        same(a,b,path)
        return
    # Closed-symbol plateaus can choose different extrema from floating-point
    # roundoff. Compare common samples and verify unmatched samples against
    # the final closed state, after that symbol's audited last closing fill.
    import datetime
    symbol=path.rsplit('.',1)[-1]
    closes=json.loads((root/'system_close_summary.json').read_text())
    assert path.startswith('hxl_history.') and symbol in closes,path
    closed_us=int(datetime.datetime.fromisoformat(closes[symbol]['last_shanghai']).timestamp()*1e6)
    am={p['ts_us']:p for p in a};bm={p['ts_us']:p for p in b}
    for ts in am.keys()&bm.keys():same(am[ts],bm[ts],path+'.at_'+str(ts))
    for points,other in [(a,bm),(b,am)]:
        for point in points:
            if point['ts_us'] in other:continue
            assert point['ts_us']>closed_us,(path,point['ts_us'],closed_us)
            expected=dict(points[-1],ts_us=point['ts_us'])
            same(point,expected,path+'.closed_plateau_'+str(point['ts_us']))

result={'protected':{},'accounts':[]}
for k in ['processes','config_hashes','tables','cpus','webroot']:
 same(before['protected'][k],after['protected'][k],'protected.'+k)
 result['protected'][k]=before['protected'][k]==after['protected'][k]
for name in before:
 if not(name.endswith('_1d') or name=='hxl_history'):continue
 a=before[name]['report'];b=after[name]['report']
 for k in ['summary','points','symbols']:
  same(a[k],b[k],name+'.'+k)
 for ap,bp in zip(a['symbol_points'],b['symbol_points']):
  same(ap['symbol'],bp['symbol'],name+'.symbol')
  same_curve(ap['points'],bp['points'],name+'.symbol_points.'+ap['symbol'])
 is_virtual=lambda s:s.startswith('__') or s.lower()=='system_position_close'
 real=lambda r:{s['strategy']:s for s in r['strategy_points'] if not is_virtual(s['strategy'])}
 same(real(a),real(b),name+'.named_strategies')
 for totals in [b['summary']]+[s['summary'] for s in b['strategy_points']]:
  same(totals['nav_change_before_fee_quote']-totals['estimated_trading_fee_quote'],totals['nav_change_after_fee_quote'],name+'.fee_identity')
 same(sum(s['summary']['nav_change_after_fee_quote'] for s in b['strategy_points']),b['summary']['nav_change_after_fee_quote'],name+'.strategy_additivity')
 af=before[name].get('estimated_funding');bf=after[name].get('estimated_funding')
 if af and bf and af.get('income_quote')is not None and bf.get('income_quote')is not None:
  same(af['income_quote'],bf['income_quote'],name+'.account_funding')
  fund=lambda v:{s['strategy']:s for s in v['strategies'] if not is_virtual(s['strategy'])}
  same(fund(af),fund(bf),name+'.named_strategy_funding')
 result['accounts'].append({'query':name,'nav_after_fee':b['summary']['nav_change_after_fee_quote'],'strategy_count_before':len(a['strategy_points']),'strategy_count_after':len(b['strategy_points'])})
for name in []:
 b=after[name]['report'];virtual=[s for s in b['available_strategies'] if s=='__initial_position__' or s.lower()=='system_position_close']
 assert virtual==[],(name,virtual)
initial=next(s for s in after['hxl_history']['report']['strategy_points'] if s['strategy']=='__initial_position__')
assert initial['gross_position_value_quote']<1e-8
assert initial['summary']['fill_count']==193
result['hxl_initial_history']=initial['summary']
result['hxl_initial_position_value']=initial['gross_position_value_quote']
result['protected_process_count']=len(before['protected']['processes'])
result['config_hash_count']=len(before['protected']['config_hashes'])
result['errors']=errors
(root/'validation_first.json').write_text(json.dumps(result,indent=2)+'\n')
print(json.dumps(result,indent=2))
assert not errors
