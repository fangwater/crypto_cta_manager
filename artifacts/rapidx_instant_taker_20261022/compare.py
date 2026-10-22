"""Compare fixed historical responses and independently check reference invariants."""
import json
import math
from pathlib import Path

p = Path(__file__).parent
raw = Path('/home/fanghaizhou/crypto_cta_manager-archives/20261022-rapidx-instant-taker')
before = json.loads((raw/'before.json').read_text())
after = json.loads((raw/'after.json').read_text())
assert before['window'] == after['window']
result = {'factual': {}, 'existing_references': {}, 'availability': {}, 'cost': {}}


def stable(value):
    if isinstance(value, dict):
        return {k: stable(v) for k, v in value.items() if k not in ['generated_at_us', 'available_from_us', 'reference_prices']}
    if isinstance(value, list):
        return [stable(v) for v in value]
    return value


for key, old in before['queries'].items():
    new = after['queries'][key]
    expected = ['twap5m'] if 'trade03' in key else ['vwap3m', 'twap3m', 'instant_taker']
    if not key.endswith('_cost'):
        assert old['report'] == new['report'], 'factual changed: '+key
        result['factual'][key] = True
        references = new['theoretical']
        assert [t['execution_model'] for t in references] == expected
        assert len(new['execution_slippage']) == len(expected)
        for i, previous in enumerate(old['theoretical']):
            assert stable(previous) == stable(references[i]), (key, 'existing theory changed', i)
            assert old['execution_slippage'][i] == new['execution_slippage'][i], (key, 'existing slippage changed', i)
        result['existing_references'][key] = True
        result['availability'][key] = []
        for t in references:
            assert t['points'] and not t['loading'] and not t['unavailable_reason'] and t['missing_price_count'] == 0
            assert t['points'][0]['nav_change_before_fee_quote'] == 0
            assert t['points'][0]['nav_change_after_fee_quote'] == 0
            for point in t['points']:
                assert math.isclose(point['nav_change_before_fee_quote'] - point['nav_change_after_fee_quote'], point['estimated_trading_fee_quote'], rel_tol=1e-10, abs_tol=1e-8)
            result['availability'][key].append({'model': t['execution_model'], 'points': len(t['points']), 'missing': 0})
        continue
    reports = new['reports']
    assert [r['execution_model'] for r in reports] == expected
    for i, previous in enumerate(old['reports']):
        assert stable(previous) == stable(reports[i]), (key, 'existing cost changed', i)
    result['existing_references'][key] = True
    summary = []
    for report in reports:
        totals = report['totals']
        assert totals['missing_virtual_delta_count'] == totals['pending_virtual_delta_count'] == 0
        for field in ['actual_fill_count', 'actual_turnover_usdt', 'actual_fee_usdt']:
            assert totals[field] == old['reports'][0]['totals'][field], (key, field)
        for row in report['rows']:
            assert [r['execution_model'] for r in row['reference_prices']] == expected
            own = next(r['price'] for r in row['reference_prices'] if r['execution_model'] == row['execution_model'])
            assert own == row['virtual_vwap']
            if row['execution_model'] == 'instant_taker':
                assert row['sample_prices'] == [own] and row['sample_quantity_weights'] == [1]
                assert row['execution_window_secs'] == 0
                assert row['virtual_execution_ts_us'] == row['received_at_us']
            elif row['execution_model'] != 'twap5m':
                price = sum((v or 0)*w for v,w in zip(row['sample_prices'], row['sample_quantity_weights'], strict=True))
                assert math.isclose(price, own, rel_tol=1e-12, abs_tol=1e-10)
        summary.append({'model': report['execution_model'], 'rows': len(report['rows']), 'matched_fills': totals['matched_fill_count'], 'missing': 0})
    result['cost'][key] = summary
(p/'comparison_summary.json').write_text(json.dumps(result, indent=2)+'\n')
print(json.dumps(result))
