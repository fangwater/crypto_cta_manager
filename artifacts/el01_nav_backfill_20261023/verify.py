"""Run read-only NAV checks through el01's existing authenticated Nginx gateway."""
import json
import subprocess
from pathlib import Path

out = Path(__file__).resolve().parent
common = (out.parent / 'el01_multiple_virtuals_20261022/remote_common.py').read_text()
baseline = {
    row['name']: row['body']
    for row in map(json.loads, (out / 'cached_before.jsonl').read_text().splitlines())
    if row['name'] in ['timeline', 'account-timeline']
}
window = json.loads((out / 'cached_window.json').read_text())
body = '\nbaseline=' + repr(baseline) + '\ncached_window=' + repr(window) + r'''
def check_report(report):
    assert report['selected_source_ids'] == ['binance_exec_trade04']
    assert report['valuation'] == 'quantity_fifo_closed_1m_window_delta'
    assert len(report['points']) > 1
    first, last = report['points'][0], report['points'][-1]
    assert first['ts_us'] == report['start_ts_us']
    assert last['ts_us'] == report['end_ts_us']
    for metric in ['nav_change_before_fee_quote','nav_change_after_fee_quote']:
        assert abs(first[metric]) < 1e-8
    for metric, value in report['summary'].items():
        assert abs(last[metric]-value) < 1e-7, (metric,last[metric],value)
    for point in report['points']:
        assert abs(point['nav_change_before_fee_quote'] - point['nav_change_after_fee_quote'] - point['estimated_trading_fee_quote']) < 1e-7
    for metric in ['nav_change_before_fee_quote','nav_change_after_fee_quote','estimated_trading_fee_quote']:
        assert abs(sum(s[metric] for s in report['symbols'])-report['summary'][metric]) < 1e-7
        if report['strategy_points']:
            assert abs(sum(s['summary'][metric] for s in report['strategy_points'])-report['summary'][metric]) < 1e-7

with gateway() as get:
    emit('health',get('health'))
    for endpoint in ['timeline','account-timeline']:
        result=get(endpoint+'?'+urllib.parse.urlencode(cached_window))
        report=result['report']
        check_report(report)
        assert report == baseline[endpoint], endpoint+' changed cached NAV'
        emit('cached_comparison',dict(endpoint=endpoint,identical=True,points=len(report['points'])))
    end=(int(time.time())//60)*60000
    window=dict(startMs=end-86400000,endMs=end,maxPoints=3500,includeTheoretical='false',sourceIds='binance_exec_trade04')
    emit('fresh_window',window)
    started=time.monotonic()
    for endpoint in ['timeline','account-timeline']:
        while True:
            began=time.monotonic()
            try:
                result=get(endpoint+'?'+urllib.parse.urlencode(window))
                break
            except urllib.error.HTTPError as error:
                reason=json.loads(error.read())
                emit('pending',dict(endpoint=endpoint,http=error.code,reason=reason))
                emit('kline_status',get('catalog/kline-status'))
                assert error.code==503 and reason.get('loading') and time.monotonic()-started < 600
                time.sleep(10)
        report=result['report']
        check_report(report)
        emit('fresh_nav',dict(endpoint=endpoint,duration_ms=round((time.monotonic()-began)*1000),points=len(report['points']),summary=report['summary'],valuation=report['valuation']))
        again=get(endpoint+'?'+urllib.parse.urlencode(window))['report']
        assert again == report
        emit('repeat_identical',dict(endpoint=endpoint,identical=True))
    # Trigger the normal full overlay after factual NAV, matching the browser.
    window['includeTheoretical']='true'
    full=get('timeline?'+urllib.parse.urlencode(window))
    check_report(full['report'])
    emit('overlay',dict(theoretical=[dict(model=t.get('execution_model'),loading=t.get('loading'),points=len(t.get('points',[])),reason=t.get('unavailable_reason')) for t in full.get('theoretical',[])],funding_loading=full.get('estimated_funding',{}).get('loading')))
    for cycle in range(4):
        next_minute=(int(time.time())//60+1)*60+2
        emit('awaiting_new_minute',dict(cycle=cycle,at_ms=next_minute*1000))
        while time.time() < next_minute:
            time.sleep(min(20,next_minute-time.time()))
        end=int(time.time()*1000)
        live=dict(startMs=end-86400000,endMs=end,maxPoints=3500,includeTheoretical='false',sourceIds='binance_exec_trade04')
        for attempt in range(4):
            began=time.monotonic()
            try:
                result=get('timeline?'+urllib.parse.urlencode(live))
                break
            except urllib.error.HTTPError as error:
                reason=json.loads(error.read())
                emit('live_pending',dict(cycle=cycle,attempt=attempt,http=error.code,reason=reason))
                assert error.code==503 and reason.get('loading') and attempt < 3
                time.sleep(10)
        report=result['report']
        check_report(report)
        emit('live_minute',dict(cycle=cycle,endMs=end,attempt=attempt,duration_ms=round((time.monotonic()-began)*1000),points=len(report['points'])))
        emit('live_status',get('catalog/kline-status'))
    live['includeTheoretical']='true'
    full=get('timeline?'+urllib.parse.urlencode(live))
    check_report(full['report'])
    emit('final_overlay',dict(theoretical=[dict(model=t.get('execution_model'),loading=t.get('loading'),points=len(t.get('points',[])),reason=t.get('unavailable_reason')) for t in full.get('theoretical',[])],funding_loading=full.get('estimated_funding',{}).get('loading')))
    assert all(not t.get('loading') and len(t.get('points',[])) > 0 for t in full['theoretical'])
    # The public funding-rate cache has its own cold-start work after restart.
    for _ in range(12):
        if not full.get('estimated_funding',{}).get('loading'):
            break
        time.sleep(10)
        full=get('timeline?'+urllib.parse.urlencode(live))
        check_report(full['report'])
    assert not full.get('estimated_funding',{}).get('loading')
    emit('kline_status_end',get('catalog/kline-status'))
emit('protected_after',protected())
'''
with (out / 'after_final.jsonl').open('w') as log:
    process = subprocess.Popen(['ssh', 'cta_exec', 'python3', '-'], stdin=subprocess.PIPE,
                               stdout=subprocess.PIPE, text=True)
    process.stdin.write(common + body)
    process.stdin.close()
    for line in process.stdout:
        log.write(line)
        log.flush()
        row = json.loads(line)
        if row['name'] == 'protected_after':
            print('Captured protected state after deployment', flush=True)
        else:
            print(line, end='', flush=True)
    raise SystemExit(process.wait())
