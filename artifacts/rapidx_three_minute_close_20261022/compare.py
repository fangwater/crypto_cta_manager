import json, math
from pathlib import Path
p=Path(__file__).parent
raw=Path('/home/fanghaizhou/crypto_cta_manager-archives/20261022-rapidx-three-minute')
before=json.loads((raw/'before.json').read_text());after=json.loads((raw/'after_close.json').read_text())
result={'factual':{},'native_theory':{},'cost':{},'availability':{}}
for key,old in before['queries'].items():
    new=after['queries'][key]
    if not key.endswith('_cost'):
        assert old['report']==new['report'], 'factual changed: '+key
        result['factual'][key]=True
        references=new['theoretical']
        expected=['twap5m'] if 'trade03' in key else ['vwap3m','twap3m']
        assert [t['execution_model'] for t in references]==expected
        assert len(new['execution_slippage'])==len(expected)
        result['availability'][key]=[]
        for t in references:
            assert t['points'] and not t['loading'] and not t['unavailable_reason'] and t['missing_price_count']==0
            if t['points']:
                assert not t['unavailable_reason']
                assert t['points'][0]['nav_change_before_fee_quote']==0
            else:
                assert t['unavailable_reason'] and ('无成交分钟' in t['unavailable_reason'] or '总成交量为零' in t['unavailable_reason'])
            result['availability'][key].append({'model':t['execution_model'],'points':len(t['points']),'reason':t['unavailable_reason']})
        if 'trade03' in key:
            for name,value in old['theoretical'].items():
                if name != 'available_from_us':assert references[0][name]==value,(key,name) # moving retention boundary
            assert new['execution_slippage'][0]==old['execution_slippage']
            result['native_theory'][key]=True
        continue
    old_report=old['report'];reports=new['reports']
    if 'trade03' in key:
        def without_added(value):
            if isinstance(value,dict):return {k:without_added(v) for k,v in value.items() if k not in ['generated_at_us','execution_model','execution_window_secs','reference_prices','sample_quantity_weights']}
            if isinstance(value,list):return [without_added(v) for v in value]
            return value
        assert without_added(old_report)==without_added(reports[0]),'native cost changed'
    summary=[]
    for report in reports:
        totals=report['totals']
        assert totals['missing_virtual_delta_count']==0
        assert totals['pending_virtual_delta_count']==0
        for field in ['actual_fill_count','actual_turnover_usdt','actual_fee_usdt']:
            assert math.isclose(totals[field],old_report['totals'][field],rel_tol=1e-12,abs_tol=1e-9),(key,field)
        checked=0
        for row in report['rows']:
            if row['execution_model']=='twap5m':continue
            prices=row['sample_prices'];weights=row['sample_quantity_weights']
            assert len(prices)==len(weights)==3 and row['execution_window_secs']==180
            expected=sum(p for p in prices if p is not None)/3 if row['execution_model']=='twap3m' else sum((p or 0)*w for p,w in zip(prices,weights))
            assert math.isclose(row['virtual_vwap'],expected,rel_tol=1e-12,abs_tol=1e-10)
            assert math.isclose(sum(weights),1,rel_tol=1e-12)
            if row['execution_model']=='twap3m':assert weights==[1/3]*3
            expected_end=((row['received_at_us']+60_000_000-1)//60_000_000+3)*60_000_000
            assert row['virtual_execution_ts_us']==expected_end
            assert [r['execution_model'] for r in row['reference_prices']]==['vwap3m','twap3m']
            own=next(r['price'] for r in row['reference_prices'] if r['execution_model']==row['execution_model'])
            assert own==row['virtual_vwap']
            checked+=1
        summary.append({'model':report['execution_model'],'checked_prices':checked,'missing':totals['missing_virtual_delta_count'],'matched_fills':totals['matched_fill_count'],'price_shortfall_bps':totals['price_shortfall_bps']})
    result['cost'][key]=summary
(p/'comparison_summary.json').write_text(json.dumps(result,indent=2)+'\n')
print(json.dumps(result))
