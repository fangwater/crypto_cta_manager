import sys
STAGE = ROOT/'deploy-staging/trade05_allocation_20261021'
phase=sys.argv[1]
end_ms=int(sys.argv[2])
token=secrets.token_urlsafe(32)
digest=hashlib.sha256(token.encode()).hexdigest()
session_id=sql("INSERT INTO cta_auth_sessions (user_id,token_hash,expires_at) SELECT user_id,decode('"+digest+"','hex'),now()+interval '30 minutes' FROM cta_users WHERE role='admin' AND disabled=false ORDER BY user_id LIMIT 1 RETURNING session_id;",readonly=False)
assert session_id.isdigit()
opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
def get(name,path):
    began=time.monotonic()
    while True:
        try:
            with opener.open(urllib.request.Request('http://13.115.227.29:4191'+path,headers={'Cookie':'cta_session='+token}),timeout=55) as response:
                body=json.load(response)
            emit(name,body)
            return body
        except urllib.error.HTTPError as error:
            try: payload=json.loads(error.read())
            except ValueError: payload={'error':'gateway unavailable'}
            if (error.code in [502,504] or error.code==503 and payload.get('loading')) and time.monotonic()-began<500:
                emit('loading',{'request':name,'elapsed_s':time.monotonic()-began})
                time.sleep(10)
                continue
            emit(name+'_error',{'status':error.code,'body':payload})
            return None
try:
    get('health','/manager/api/health')
    dashboard=get('dashboard','/manager/api/dashboard')
    for source in dashboard['report']['sources']:
        if source.get('first_fill_ts_us') is None: continue
        start=max(end_ms-86_400_000,((source.get('initial_position_snapshot_ts_us') or source['first_fill_ts_us'])+999)//1000)
        for endpoint in ['timeline','account-timeline']:
            params=dict(startMs=start,endMs=end_ms,maxPoints=200,sourceIds=source['source_id'],includeTheoretical='false')
            get(source['source_id']+'_'+endpoint,'/manager/api/'+endpoint+'?'+urllib.parse.urlencode(params))
    for days in [7]:
        params=dict(startMs=end_ms-days*86_400_000,endMs=end_ms,maxPoints=3500,sourceIds=SOURCE,includeTheoretical='false')
        get('trade05_'+str(days)+'d','/manager/api/account-timeline?'+urllib.parse.urlencode(params))
    # Warm through the latest completed candles on Manager's existing egress.
    now_ms=int(time.time()*1000)
    params=dict(startMs=now_ms-3_600_000,endMs=now_ms,maxPoints=20,sourceIds=SOURCE,includeTheoretical='true')
    get('warm_latest','/manager/api/timeline?'+urllib.parse.urlencode(params))
    if phase != 'before':
        observed=json.loads((STAGE/'before_exec_snapshot.json').read_text())
        entry=next(e['entry'] for e in observed['entries'] if e['channel']=='exec_pre_trade_state')
        stamp=entry['ts_ms']
        params=dict(startMs=stamp,endMs=stamp+1,maxPoints=20,sourceIds=SOURCE,includeTheoretical='false')
        get('alignment_boundary','/manager/api/account-timeline?'+urllib.parse.urlencode(params))
    snapshot=get('exec_snapshot','/exec_trade05/snapshot')
    (STAGE/(phase+'_exec_snapshot.json')).write_text(json.dumps(snapshot))
    get('kline_status','/manager/api/catalog/kline-status')
finally:
    sql('DELETE FROM cta_auth_sessions WHERE session_id='+session_id+';',readonly=False)
    emit('diagnostic_session_removed',True)
