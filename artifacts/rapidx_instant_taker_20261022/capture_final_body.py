import sys
phase = sys.argv[1]
OUT = ROOT / 'deploy-staging/rapidx_instant_taker_20261022'
OUT.mkdir(parents=True, exist_ok=True)
window_file = OUT / 'window.json'
if phase == 'before':
    end = (int(time.time()) // 60 - 10) * 60000
    window_file.write_text(json.dumps({'startMs':end - 6*3600000,'endMs':end}))
window = json.loads(window_file.read_text())
token = secrets.token_urlsafe(32)
digest = hashlib.sha256(token.encode()).hexdigest()
session_id = sql("INSERT INTO cta_auth_sessions (user_id,token_hash,expires_at) SELECT user_id,decode('"+digest+"','hex'),now()+interval '30 minutes' FROM cta_users WHERE role='admin' AND disabled=false ORDER BY user_id LIMIT 1 RETURNING session_id;", readonly=False)
assert session_id.isdigit()
opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
def get(path):
    began=time.monotonic()
    while True:
        try:
            with opener.open(urllib.request.Request('http://13.115.227.29:4191'+path,headers={'Cookie':'cta_session='+token}),timeout=55) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            if error.code not in [502,503,504] or time.monotonic()-began>300:raise
            time.sleep(5)
try:
    emit('health',get('/manager/api/health'))
    data={'window':window,'queries':{}}
    for source in ['binance_exec_trade03','binance_exec_trade05','binance_exec_trade06','binance_exec_trade07']:
        for endpoint in ['timeline','account-timeline']:
            params=dict(window,maxPoints=300,sourceIds=source,includeTheoretical='true')
            began=time.monotonic()
            while True:
                try:
                    body=get('/manager/api/'+endpoint+'?'+urllib.parse.urlencode(params))
                    theory=body['theoretical']
                    references=theory if isinstance(theory,list) else [theory]
                    if not any(t['loading'] for t in references):break
                except urllib.error.HTTPError as error:
                    if error.code not in [502,503,504]:raise
                assert time.monotonic()-began<900,'coverage still pending'
                time.sleep(10)
            data['queries'][source+'_'+endpoint]=body
            emit('query',{'source':source,'endpoint':endpoint,'elapsed_s':round(time.monotonic()-began,2),'references':[{'model':t.get('execution_model'),'points':len(t['points']),'missing':t['missing_price_count'],'reason':t['unavailable_reason']} for t in references]})
        params=dict(startMs=window['endMs']-3600000,endMs=window['endMs'],sourceIds=source,pageSize=100)
        began=time.monotonic()
        while True:
            body=get('/manager/api/catalog/acquisition-cost?'+urllib.parse.urlencode(params))
            if not any(r['warnings'] or r['totals']['missing_virtual_delta_count'] or r['totals']['pending_virtual_delta_count'] for r in body['reports']):break
            assert time.monotonic()-began<600,'cost coverage still pending'
            time.sleep(5)
        data['queries'][source+'_cost']=body
        reports=body.get('reports') or [body['report']]
        emit('cost',{'source':source,'reports':[{'model':r.get('execution_model'),'rows':len(r['rows']),'missing':r['totals']['missing_virtual_delta_count']} for r in reports]})
    data['kline_status']=get('/manager/api/catalog/kline-status')
    (OUT/(phase+'.json')).write_text(json.dumps(data))
    emit('saved',{'phase':phase,'window':window})
finally:
    sql('DELETE FROM cta_auth_sessions WHERE session_id='+session_id+';',readonly=False)
    emit('diagnostic_session_removed',True)
