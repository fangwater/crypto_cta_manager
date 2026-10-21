import json
from pathlib import Path
p=Path(__file__).parent
def rows(f):return {r['name']:r['body'] for x in (p/f).read_text().splitlines() if (r:=json.loads(x)).get('name')}
a=rows('before.jsonl');b=rows('after.jsonl');e=rows('extra.jsonl');s=rows('strategies.jsonl')
base=a['trade01_baseline']['report'];target=b['trade01_first_day']['report'];start=base['start_ts_us']
qty={x['symbol']:x['net_quantity'] for x in base['symbols']};initial=qty.copy()
marks={(x['symbol'],x['ts_us']):x['close'] for f in ['marks.jsonl','verify_marks.jsonl'] for line in (p/f).read_text().splitlines() if (x:=json.loads(line))}
fills=[]
for l in (p/'stores.txt').read_text().splitlines():
 r=l.split('\t')
 if len(r)==6 and r[0].isdigit() and start<=int(r[0])<=target['end_ts_us']:fills.append((int(r[0]),r[1],float(r[2]),float(r[3])))
fills.sort(key=lambda r:r[0]);cursor=0;cash=0;maxerr=0;out=[]
for point in target['points']:
 ts=point['ts_us']
 while cursor<len(fills) and fills[cursor][0]<=ts:
  _,symbol,q,price=fills[cursor];qty[symbol]+=q;cash-=q*price;cursor+=1
 expected=cash+sum(qty[symbol]*marks[symbol,ts]-initial[symbol]*marks[symbol,start] for symbol in qty)
 err=abs(expected-point['nav_change_before_fee_quote']);maxerr=max(maxerr,err)
 out.append(dict(ts_us=ts,independent_before=expected,live_before=point['nav_change_before_fee_quote'],error=err))
assert maxerr<1e-7,maxerr
r=e['trade01_repeat']['report'];maxstrategy=0
for st in r['strategy_points']:
 if st['strategy'] not in s:continue
 for k,v in st['summary'].items():maxstrategy=max(maxstrategy,abs(v-s[st['strategy']]['totals'][k]))
assert maxstrategy<1e-7,maxstrategy
curves=[{p['ts_us']:p for p in st['points']} for st in target['strategy_points']]
maxadd=max(abs(sum(st[point['ts_us']][key] for st in curves)-point[key]) for point in target['points'] for key in ['nav_change_before_fee_quote','nav_change_after_fee_quote','estimated_trading_fee_quote'])
assert maxadd<1e-7,maxadd
max_terminal_add=max(abs(sum(st['summary'][key] for st in r['strategy_points'])-r['summary'][key]) for key in r['summary'])
assert max_terminal_add<1e-7,max_terminal_add
result=dict(first_day_replay_max_error=maxerr,first_day_points=len(out),strategy_export_max_error=maxstrategy,strategy_additivity_first_day_max_error=maxadd,strategy_terminal_sum_max_error=max_terminal_add,first_day_end=out[-1])
(p/'independent_live_comparison.json').write_text(json.dumps(dict(summary=result,points=out),indent=2))
print(json.dumps(result,indent=2))
