"""Reproduce funding reconciliation and the RapidX cutoff/price evidence offline."""
from collections import defaultdict
from decimal import Decimal
import json
from pathlib import Path

root = Path(__file__).parent
reports = []
for host in ['el01', 'jp_meta']:
    for report in json.loads((root / (host + '_comparison.json')).read_text())['reports']:
        if not report['allocation']:
            reports.append(report)
positions = json.loads((root / 'rapidx_positions.json').read_text())['positions']
shared_prices = defaultdict(list)
big_cases = []
for report in reports:
    if not report['source_id'].endswith(('05', '06', '07')):
        continue
    for row in report['comparisons']:
        if not row['observed'] or not row['actual'] or not row['quantity'] or not row['rate']:
            continue
        qty = row['quantity']
        if report['source_id'].endswith('06') and abs(row['error']) > 0.0001:
            delta = sum(f['signed_qty'] for f in row['boundary_fills']
                        if 0 <= f['fill_ts_us'] - row['ts_us'] <= 15_000_000)
            qty += delta
            big_cases.append({'symbol': row['symbol'], 'settlement_us': row['ts_us'],
                              'public_time_quantity': row['quantity'], 'later_quantity': qty,
                              'actual_income': row['actual'], 'original_error': row['error'],
                              'error_after_quantity_adjustment': -qty * row['mark_price'] * row['rate'] - row['actual']})
        price = -Decimal(str(row['actual'])) / Decimal(str(qty)) / Decimal(str(row['rate']))
        shared_prices[(row['symbol'], row['ts_us'])].append((report['source_id'], float(price)))
multi = [{'symbol': symbol, 'ts_us': ts, 'implied_marks': values}
         for (symbol, ts), values in shared_prices.items() if len(values) > 1]
assert len(multi) == 31
assert all(max(p for _, p in x['implied_marks']) - min(p for _, p in x['implied_marks']) < 1e-7 for x in multi)
assert -Decimal('1273') * Decimal('0.2354') * Decimal('0.00007819') == Decimal('-0.023430743798')
assert -Decimal('-346.3') * Decimal('1.1569') * Decimal('0.00001563') == Decimal('0.0062619167661')
aave = [p for p in positions if p['source_id'].endswith('05')
        and p['settlement_us'] == 1791993600000000
        and p['row']['sym'] == 'BINANCE_PERP_AAVE_USDT'
        and p['row']['markPrice'] == '155.99']
assert len(aave) == 1
out = {'reconciliation': [{'source_id': r['source_id'], **r['reconciliation']} for r in reports],
       'large_differences': big_cases, 'shared_mark_groups': multi, 'aave_observed_mark': aave}
(root / 'analysis.json').write_text(json.dumps(out, indent=2) + '\n')
print(json.dumps({'accounts': len(reports), 'consistent_shared_mark_groups': len(multi),
                  'exact_large_case_reproductions': 2, 'observed_aave_mark': aave[0]['row']['markPrice']}))
