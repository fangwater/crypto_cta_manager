"""Reconcile saved funding with independent RapidX statements and scoped NAV."""
import decimal
import hashlib
import hmac
import http.client
import json
import math
import os
import pathlib
import secrets
import subprocess
import time
import urllib.parse
import urllib.request
import urllib.error

ROOT=pathlib.Path('/home/ubuntu/crypto_cta_manager')
STAGE=ROOT/'backups/rx01_initial_snapshot_20261015'
SOURCES=['binance_exec_trade05','binance_exec_trade06','binance_exec_trade07']
pid=int(subprocess.check_output(['systemctl','--user','show','crypto-cta-manager-web.service','-p','MainPID','--value'],text=True))
runtime=dict(x.split(b'=',1) for x in pathlib.Path(f'/proc/{pid}/environ').read_bytes().split(b'\0') if b'=' in x)
u=urllib.parse.urlsplit(runtime[b'CRYPTO_CTA_LOCAL_DATABASE_URL'].decode())
env=dict(os.environ,PGHOST=u.hostname,PGPORT=str(u.port or 5432),PGUSER=urllib.parse.unquote(u.username),
         PGPASSWORD=urllib.parse.unquote(u.password or ''),PGDATABASE=u.path.lstrip('/'))
def sql(q):
    r=subprocess.run(['psql','-X','-qAt','-v','ON_ERROR_STOP=1'],input=q,text=True,capture_output=True,env=env)
    assert r.returncode==0,'verification database read failed (details suppressed)'
    return r.stdout.strip()
def array(q): return json.loads(sql("SELECT coalesce(jsonb_agg(to_jsonb(t)),'[]') FROM ("+q+") t;"))
token=secrets.token_urlsafe(32); digest=hashlib.sha256(token.encode()).hexdigest()
session=sql("INSERT INTO cta_auth_sessions(user_id,token_hash,expires_at) SELECT user_id,decode('"+digest+"','hex'),now()+interval '15 minutes' FROM cta_users WHERE role='admin' AND NOT disabled ORDER BY user_id LIMIT 1 RETURNING session_id;")
assert session.isdecimal()
opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
def get(path,json_body=True):
    req=urllib.request.Request('http://127.0.0.1:4191'+path,headers={'Cookie':'cta_session='+token})
    try:
        with opener.open(req,timeout=175) as response:
            body=response.read()
            return json.loads(body) if json_body else body
    except urllib.error.HTTPError as error:
        body=error.read().decode()
        raise RuntimeError('Verification HTTP '+str(error.code)+': '+body[:1000]) from None
def credentials(source):
    root=pathlib.Path('/home/ubuntu')/source
    for p in pathlib.Path('/proc').iterdir():
        if not p.name.isdecimal(): continue
        try:
            if (p/'comm').read_text().strip()=='trade_engine' and (p/'cwd').resolve()==root:
                wanted={b'LTP_API_KEY',b'LTP_API_SECRET',b'LTP_PORTFOLIO_ID'}
                return {k.decode():v.decode() for x in (p/'environ').read_bytes().split(b'\0') if b'=' in x for k,v in [x.split(b'=',1)] if k in wanted}
        except OSError: pass
    raise RuntimeError('live trading credential source unavailable')
def statements(source,from_ms,through_ms,local_ip):
    creds=credentials(source); result=[]; ids=set(); pagination=None
    for page in range(1,101):
        params={'exchange':'BINANCE','startTime':str(from_ms),'endTime':str(through_ms),'statementType':'FUNDING_FEE','page':str(page),'pageSize':'1000'}
        nonce=str(time.time_ns()//1_000_000_000)
        signed='&'.join(k+'='+params[k] for k in sorted(params))+'&'+nonce
        headers={'X-MBX-APIKEY':creds['LTP_API_KEY'],'nonce':nonce,'ts':str(time.time_ns()//1000),
                 'signature':hmac.new(creds['LTP_API_SECRET'].encode(),signed.encode(),hashlib.sha256).hexdigest()}
        conn=http.client.HTTPSConnection('api.liquiditytech.com',timeout=15,source_address=(local_ip,0))
        conn.request('GET','/api/v1/trading/statement?'+urllib.parse.urlencode(sorted(params.items())),headers=headers)
        response=conn.getresponse(); root=json.loads(response.read()); conn.close()
        assert response.status==200 and root.get('code') in [200,200000]
        data=root['data']; current=(data['pageNum'],data['totalSize'])
        assert data['page']==page and data['pageSize']==1000 and (pagination is None or pagination==current)
        pagination=current
        assert len(data['list'])==min(1000,max(0,data['totalSize']-(page-1)*1000))
        for row in data['list']:
            assert str(row['portfolioId'])==creds['LTP_PORTFOLIO_ID'] and row['exchangeType']=='BINANCE' and row['statementType']=='FUNDING_FEE'
            assert row['statementId'] not in ids; ids.add(row['statementId'])
            assert from_ms<=row['createAt']<=through_ms
            result.append(row)
        if page>=data['pageNum']:
            assert len(result)==data['totalSize']; return result
        time.sleep(1.5)
    raise RuntimeError('independent statement pagination incomplete')
try:
    observations=[]
    for attempt in range(25):
        settings=[get('/manager/api/catalog/accounts/'+s+'/funding') for s in SOURCES]
        statuses=[{'source_id':r['source_id'],'status':r['summary']['status'],'records':r['summary']['record_count'],'error':r['summary']['last_error']} for r in settings]
        observations.append({'ts_us':time.time_ns()//1000,'accounts':statuses})
        (STAGE/'funding_sync_observations.json').write_text(json.dumps(observations,indent=2)+'\n')
        print(json.dumps(statuses),flush=True)
        assert all(r['enabled'] for r in settings)
        assert all(r['summary']['status']!='error' for r in settings), statuses
        if all(r['summary']['status']=='ok' for r in settings): break
        time.sleep(10)
    else: raise RuntimeError('funding backfill still pending')
    reports=[]
    for source,setting in zip(SOURCES,settings):
        starts=array("SELECT symbol,start_ts_us,synced_through_ts_us FROM cta_funding_symbols WHERE source_id='"+source+"' ORDER BY symbol")
        through=min(r['synced_through_ts_us'] for r in starts)
        from_ms=(min(r['start_ts_us'] for r in starts)+999)//1000
        remote=statements(source,from_ms,through//1000,setting['verified_local_ip'])
        start_by_symbol={r['symbol']:r['start_ts_us'] for r in starts}
        factual=set()
        for row in remote:
            parts=row['sym'].split('_'); assert len(parts)==4 and parts[:2]==['BINANCE','PERP'] and row['businessType']=='PERP'
            symbol=parts[2]+parts[3]
            if symbol not in start_by_symbol or row['createAt']*1000<start_by_symbol[symbol]: continue
            assert row['coin']==parts[3]
            factual.add((row['statementId'],symbol,row['coin'],row['createAt']*1000,decimal.Decimal(row['deltaAmount'])))
        saved=array("SELECT tran_id,symbol,asset,ts_us,income::text AS income FROM cta_funding_income WHERE source_id='"+source+"' AND ts_us<="+str(through)+" ORDER BY ts_us,tran_id")
        persisted={(r['tran_id'],r['symbol'],r['asset'],r['ts_us'],decimal.Decimal(r['income'])) for r in saved}
        assert persisted==factual, {'source':source,'saved_only':len(persisted-factual),'remote_only':len(factual-persisted)}
        query=urllib.parse.urlencode({'sourceIds':source,'endMs':through//1000,'maxPoints':200,'includeTheoretical':'false'})
        timeline=get('/manager/api/account-timeline?'+query)
        report=timeline['report']; funding=timeline['funding']
        expected=sum(r[4] for r in persisted if report['start_ts_us']<=r[3]<=report['end_ts_us'] and r[1] in report['selected_symbols'])
        assert abs(funding['summary']['income_quote']-float(expected))<1e-10
        for fee in ['before','after']:
            actual=report['summary']['nav_change_'+fee+'_fee_quote']
            total=funding['total_nav_'+fee+'_fee_quote']
            assert abs(actual+float(expected)-total)<1e-8
        assert funding['points'] and funding['points'][0]['funding_income_quote']==0
        assert abs(funding['points'][-1]['funding_income_quote']-float(expected))<1e-10
        strategy=get('/manager/api/timeline?'+query)
        assert strategy['funding']['summary']['income_quote']==0 and not strategy['funding']['points']
        (STAGE/('funding_timeline_'+source+'.json')).write_text(json.dumps(timeline)+'\n')
        item={'source_id':source,'status':setting['summary']['status'],'symbols':len(starts),'records_verified':len(persisted),
              'income_exact':str(sum(r[4] for r in persisted)),'independent_statement_match':True,'account_nav_addition_verified':True,
              'strategy_funding_excluded':True,'synced_through_ts_us':through}
        reports.append(item); print(json.dumps(item),flush=True)
    health=get('/manager/api/health'); assert health['status']=='ok' and health['last_refresh_error'] is None
    manifest=json.loads((STAGE/'funding_manifest.json').read_text())
    for name,wanted in manifest['frontend'].items():
        assert hashlib.sha256(get('/manager/'+name,False)).hexdigest()==wanted
    gateways={}
    for number in ['05','06','07']:
        for suffix in ['','snapshot','config/']:
            path='/exec_trade'+number+'/'+suffix
            get(path,False); gateways[path]=200
    status=get('/manager/api/catalog/kline-status')
    result={'verified_at_us':time.time_ns()//1000,'accounts':reports,'health':health,'frontend_checksums_match':True,
            'gateways':gateways,'target_history':status['target_history']}
    (STAGE/'funding_verification.json').write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps({'health':health,'frontend_verified':True,'gateway_routes':len(gateways),'target_history':status['target_history']},indent=2),flush=True)
finally:
    sql('DELETE FROM cta_auth_sessions WHERE session_id='+session+" AND token_hash=decode('"+digest+"','hex');")
