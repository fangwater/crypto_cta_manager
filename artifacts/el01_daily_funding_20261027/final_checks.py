"""Check cumulative account values, served assets, and persisted fee days."""
import json
from pathlib import Path
import subprocess

OUT = Path(__file__).resolve().parent
manifest = json.loads((OUT/'manifest.json').read_text())
common = (OUT.parent/'el01_multiple_virtuals_20261022/remote_common.py').read_text()
body = '\nmanifest='+repr(manifest)+'\n'+r'''
opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
for name,expected in manifest['frontend'].items():
    with opener.open('http://127.0.0.1:10051/manager/'+name,timeout=20) as response:
        assert hashlib.sha256(response.read()).hexdigest()==expected,name
emit('frontend_assets_verified',len(manifest['frontend']))
with gateway() as get:
    emit('health',get('health'))
    for attempt in range(40):
        d=get('dashboard')
        accounts={a['source_id']:a.get('cumulative_funding') for a in d['accounts'] if a['enabled']}
        ready={k:v for k,v in accounts.items() if v and v['income_quote'] is not None}
        emit('account_progress',dict(attempt=attempt,known=len(ready),total=len(accounts)))
        if len(ready)==len(accounts):
            break
        time.sleep(10)
    emit('cumulative_accounts',accounts)
    assert len(ready)==len(accounts),'some account funding values remain unavailable'
    assert accounts['binance_exec_trade09']['kind']=='actual'
    assert all(v['kind']=='estimated' for k,v in accounts.items() if k!='binance_exec_trade09')
    stage=ROOT/'releases'/manifest['release']
    secondary=ROOT/'diagnostics'/('funding-daily-secondary-'+manifest['release'])
    secondary.parent.mkdir(exist_ok=True)
    assert not secondary.exists()
    try:
        result=subprocess.run([str(stage/'funding_daily_audit'),str(ROOT/'db'),str(secondary)],capture_output=True,text=True,check=True)
        rows=json.loads(result.stdout)
    finally:
        if secondary.exists():
            shutil.rmtree(secondary)
    assert not secondary.exists()
    assert rows,'no persisted fee days'
    emit('daily_rows',rows)
    for row in rows:
        amount=row['income_quote']
        assert abs(sum(row['symbols'].values())-amount)<1e-7
        assert abs(sum(row['strategies'].values())-amount)<1e-7
    candidates=sorted((r for r in rows if 'XRPUSDT' in r['symbols']), key=lambda r:r['day_us'],reverse=True)
    checked=set()
    for row in candidates:
        source=row['source_id']
        if source in checked:
            continue
        query=dict(sourceIds=source,symbols='XRPUSDT',startMs=row['from_us']//1000,endMs=row['through_us']//1000,maxPoints=100,includeTheoretical='false')
        for attempt in range(18):
            result=get('timeline?'+urllib.parse.urlencode(query))
            funding=result['estimated_funding']
            if not funding['loading']:
                break
            time.sleep(10)
        assert funding['income_quote'] is not None and not funding['unavailable_reason'],funding
        expected=row['symbols']['XRPUSDT']
        assert abs(expected-funding['income_quote'])<1e-7,(source,expected,funding)
        emit('daily_comparison',dict(source=source,day_us=row['day_us'],symbol='XRPUSDT',saved=expected,query=funding['income_quote']))
        checked.add(source)
        if len(checked)==5:
            break
    assert len(checked)==5
emit('secondary_removed',True)
'''
with (OUT/'final_checks.jsonl').open('w') as log:
    p=subprocess.Popen(['ssh','cta_exec','python3','-'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True)
    p.stdin.write(common+body)
    p.stdin.close()
    for line in p.stdout:
        log.write(line)
        log.flush()
        row=json.loads(line)
        if row['name']=='daily_rows':
            print('Persisted fee days: '+str(len(row['body'])),flush=True)
        else:
            print(line,end='',flush=True)
    raise SystemExit(p.wait())
