import bisect
import datetime as dt
import json
from pathlib import Path

ROOT = Path(__file__).parent

def reports(name):
    return {r['name']: r['body'] for line in (ROOT / name).read_text().splitlines()
            if (r := json.loads(line)).get('name')}

data = reports('boundary.jsonl')
full = reports('probe.jsonl')['theory']
baseline = data['baseline']['report']
first_day = data['first_day']['report']
start = baseline['start_ts_us']
initial_qty = {s['symbol']: s['net_quantity'] for s in baseline['symbols']}
initial_marks = {s['symbol']: s['net_position_value_quote'] / s['net_quantity']
                 for s in baseline['symbols'] if s['net_quantity']}
marks = {(r['symbol'], r['ts_us']): r['close']
         for line in (ROOT / 'marks.jsonl').read_text().splitlines()
         if (r := json.loads(line))}
fills, last_fills = [], {}
for line in (ROOT / 'stores.txt').read_text().splitlines():
    row = line.split('\t')
    if len(row) != 6 or not row[0].isdigit():
        continue
    ts, symbol, qty, price = int(row[0]), row[1], float(row[2]), float(row[3])
    if ts < start:
        last_fills[symbol] = ts
    else:
        fills.append((ts, symbol, qty, price))
fills.sort(key=lambda row: row[0])
baseline_delta = sum(initial_qty[s] * (marks[s, start] - initial_marks[s]) for s in initial_qty)
qty, latest = initial_qty.copy(), initial_marks.copy()
cash = 0.0
cursor = 0
points = []
replay_error = 0.0
for p in first_day['points']:
    ts = p['ts_us']
    while cursor < len(fills) and fills[cursor][0] <= ts:
        _, symbol, signed, price = fills[cursor]
        qty[symbol] += signed
        cash -= signed * price
        latest[symbol] = price
        cursor += 1
    original = cash + sum(qty[s] * latest[s] - initial_qty[s] * initial_marks[s] for s in qty)
    replay_error = max(replay_error, abs(original - p['nav_change_before_fee_quote']))
    corrected = cash + sum(qty[s] * marks[s, ts] - initial_qty[s] * marks[s, start] for s in qty)
    points.append(dict(ts_us=ts, old_before=p['nav_change_before_fee_quote'],
                       old_after=p['nav_change_after_fee_quote'], new_before=corrected,
                       new_after=corrected - p['estimated_trading_fee_quote']))
assert replay_error < 1e-7, replay_error
fee_error = max(abs(p['nav_change_before_fee_quote'] - p['nav_change_after_fee_quote']
                    - p['estimated_trading_fee_quote']) for p in full['report']['points'])
offsets = [dict(symbol=s, quantity=initial_qty[s], latest_fill_mark=initial_marks[s],
                minute_close_mark=marks[s, start], last_fill_ts_us=last_fills[s],
                baseline_difference=initial_qty[s] * (marks[s, start] - initial_marks[s]))
           for s in initial_qty]
jumps = sorted([dict(ts_us=p['ts_us'], old=p['old_before'] - prev['old_before'],
                     corrected=p['new_before'] - prev['new_before'])
                for prev, p in zip(points, points[1:])], key=lambda p: abs(p['old']), reverse=True)
summary = dict(start_ts_us=start, first_day_end_ts_us=first_day['end_ts_us'],
               baseline_price_difference=baseline_delta, first_day_original_replay_error=replay_error,
               fee_identity_max_error=fee_error, first_day_terminal=points[-1], largest_jumps=jumps[:5],
               baseline_symbols=offsets, thirty_day_actual=full['report']['summary'],
               thirty_day_theoretical=full['theoretical']['points'][-1],
               subsequent_window=data['day_2_onward']['execution_slippage'])
(ROOT / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
(ROOT / 'first_day_comparison.json').write_text(json.dumps(points) + '\n')
print(json.dumps({k:v for k,v in summary.items() if k not in ['baseline_symbols']}, indent=2))

import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
import matplotlib.dates as mdates
tz = dt.timezone(dt.timedelta(hours=8))
dates = [dt.datetime.fromtimestamp(p['ts_us'] / 1e6, tz) for p in points]
fig, ax = plt.subplots(figsize=(12, 5), layout='constrained')
for key, label, color in [('old_before','Live: latest-fill marks, before fee','#c2410c'),
                          ('new_before','Diagnostic: minute-close marks, before fee','#0369a1')]:
    ax.plot(dates, [p[key] for p in points], label=label, color=color, linewidth=1.8)
theory = [p for p in full['theoretical']['points'] if start <= p['ts_us'] <= first_day['end_ts_us']]
ax.plot([dt.datetime.fromtimestamp(p['ts_us']/1e6,tz) for p in theory],
        [p['nav_change_before_fee_quote'] for p in theory], label='Theoretical, before fee',color='#15803d',alpha=.8)
ax.axhline(0,color='#94a3b8',linewidth=.7)
ax.set(title='trade01: first day of the 30D window (2026-09-21)',ylabel='NAV change (USDT)',xlabel='Shanghai time')
ax.xaxis.set_major_formatter(mdates.DateFormatter('%m-%d %H:%M',tz=tz))
ax.grid(alpha=.2);ax.legend(loc='upper left');fig.savefig(ROOT/'first_day_comparison.png',dpi=150)
