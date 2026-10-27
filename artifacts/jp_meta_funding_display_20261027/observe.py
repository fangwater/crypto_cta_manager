"""Observe several NAV generations through Nginx without clearing known Funding."""
import json
from pathlib import Path
import subprocess

OUT=Path(__file__).resolve().parent
manifest=json.loads((OUT/'manifest.json').read_text())
common=(OUT/'remote_common.py').read_text()
body='\nmanifest='+repr(manifest)+'\n'+r'''
opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
for name,expected in manifest['frontend'].items():
    with opener.open('http://13.115.227.29:4191/manager/'+name,timeout=20) as response:
        assert hashlib.sha256(response.read()).hexdigest()==expected,name
emit('frontend_assets_verified',len(manifest['frontend']))
with gateway() as get:
    emit('health',get('health'))
    known=set()
    generations=set()
    started=None
    deadline=time.monotonic()+600
    stale_samples=0
    samples=0
    while time.monotonic()<deadline:
        d=get('dashboard')
        values={a['source_id']:a['cumulative_funding'] for a in d['accounts'] if a['enabled']}
        assert len(values)==8
        for source,value in values.items():
            assert 'refreshing' in value
            if source in known:
                assert value['income_quote'] is not None,(source,value)
            if value['income_quote'] is not None:
                known.add(source)
                assert value['end_ts_us']==value['generation_us']
                if value['generation_us']!=d['generated_at_us']:
                    assert value['refreshing']
                    stale_samples+=1
        if len(known)==8:
            if started is None:
                started=time.monotonic()
            generations.add(d['generated_at_us'])
        emit('sample',dict(generation_us=d['generated_at_us'],known=len(known),values=values))
        samples+=1
        if started is not None and time.monotonic()-started>=130 and len(generations)>=3:
            break
        time.sleep(3)
    assert len(known)==8 and len(generations)>=3 and stale_samples>0
    emit('observation',dict(samples=samples,generations=len(generations),stale_values_retained=stale_samples,known_accounts=len(known),null_regressions=0,duration_secs=round(time.monotonic()-started)))
emit('protected_final',protected())
'''
with (OUT/'observation.jsonl').open('w') as log:
    p=subprocess.Popen(['ssh','jp-meta-elvpn','python3','-'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True)
    p.stdin.write(common+body)
    p.stdin.close()
    for line in p.stdout:
        log.write(line)
        log.flush()
        row=json.loads(line)
        if row['name']=='sample':
            print('Known accounts: '+str(row['body']['known'])+'; generation '+str(row['body']['generation_us']),flush=True)
        elif row['name']=='protected_final':
            print('Final protected state captured',flush=True)
        else:
            print(line,end='',flush=True)
    raise SystemExit(p.wait())
