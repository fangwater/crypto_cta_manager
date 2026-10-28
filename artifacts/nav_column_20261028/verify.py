"""Verify fixed grid, source permissions and saved-read latency via Nginx."""
from pathlib import Path
import subprocess
out=Path(__file__).resolve().parent
script=(out/'host_common.py').read_text()+r'''
import statistics
interval=4*3600000
for username in ['lgq','prc']:
    with session(username) as get:
        overview=get('account-overview')
        ids=[row['source_id'] for row in overview['accounts']]
        pending=[]
        timings=[]
        curves=[]
        for source in ids:
            before=time.monotonic()
            value=get('account-overview/nav?sourceId='+urllib.parse.quote(source,safe=''))
            timings.append((time.monotonic()-before)*1000)
            points=value['points']
            assert value['source_id']==source
            if not points:
                pending.append(source)
                continue
            assert points[0]['pnl_quote']==0
            assert all(points[i]['ts_ms']<points[i+1]['ts_ms'] for i in range(len(points)-1))
            assert all(p['ts_ms']%interval==0 for p in points[1:])
            assert all(points[i+1]['ts_ms']==(points[i]['ts_ms']//interval+1)*interval for i in range(len(points)-1))
            assert points[-1]['ts_ms']==int(time.time()*1000)//interval*interval
            assert value['end_ms']==points[-1]['ts_ms'] and value['start_ms']==points[0]['ts_ms']
            curves.append({'source_id':source,'points':len(points),'complete':value['complete'],'loading':value['loading']})
        if username=='prc':
            try:
                get('account-overview/nav?sourceId=external%3Ajp-meta-nav%3Abinance_fr_arb02')
                raise AssertionError('hidden external source accessible')
            except urllib.error.HTTPError as error:assert error.code==403
        emit('nav_validation',{'user':username,'accounts':len(ids),'ready':len(curves),'pending':pending,'max_ms':round(max(timings),2),'median_ms':round(statistics.median(timings),2),'curves':curves})
'''
r=subprocess.run(['ssh','-o','BatchMode=yes','jp-meta-elvpn','python3','-'],input=script,text=True,capture_output=True)
(out/'validation.jsonl').write_text(r.stdout)
print(r.stdout,end='')
if r.returncode:
    print(r.stderr)
    raise SystemExit(r.returncode)
