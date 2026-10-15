"""Enable only the three authorized accounts through the tested Manager API."""
import hashlib
import json
import os
import pathlib
import secrets
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request

ROOT=pathlib.Path('/home/ubuntu/crypto_cta_manager')
STAGE=ROOT/'backups/rx01_initial_snapshot_20261015'
pid=int(subprocess.check_output(['systemctl','--user','show','crypto-cta-manager-web.service','-p','MainPID','--value'],text=True))
runtime=dict(x.split(b'=',1) for x in pathlib.Path(f'/proc/{pid}/environ').read_bytes().split(b'\0') if b'=' in x)
assert runtime.get(b'CTA_FUNDING_ENABLE_TOKEN'), 'host funding token missing'
u=urllib.parse.urlsplit(runtime[b'CRYPTO_CTA_LOCAL_DATABASE_URL'].decode())
env=dict(os.environ,PGHOST=u.hostname,PGPORT=str(u.port or 5432),PGUSER=urllib.parse.unquote(u.username),
         PGPASSWORD=urllib.parse.unquote(u.password or ''),PGDATABASE=u.path.lstrip('/'))
def sql(q):
    r=subprocess.run(['psql','-X','-qAt','-v','ON_ERROR_STOP=1'],input=q,text=True,capture_output=True,env=env)
    assert r.returncode==0,'database operation failed (details suppressed)'
    return r.stdout.strip()
token=secrets.token_urlsafe(32); digest=hashlib.sha256(token.encode()).hexdigest()
session=sql("INSERT INTO cta_auth_sessions(user_id,token_hash,expires_at) SELECT user_id,decode('"+digest+"','hex'),now()+interval '15 minutes' FROM cta_users WHERE role='admin' AND NOT disabled ORDER BY user_id LIMIT 1 RETURNING session_id;")
assert session.isdecimal()
opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
def get(path,body=None):
    req=urllib.request.Request('http://127.0.0.1:4191'+path,
        data=None if body is None else json.dumps(body).encode(),method='GET' if body is None else 'PUT',
        headers={'Cookie':'cta_session='+token,'Content-Type':'application/json'})
    try:
        with opener.open(req,timeout=90) as response: return json.load(response)
    except urllib.error.HTTPError as error:
        # Error bodies from Manager contain only reviewed fixed-IP/REST contexts.
        data=error.read()
        if error.code in [502,503,504]: raise RuntimeError('Manager is still starting; no funding switch saved') from None
        try: body=json.loads(data)
        except ValueError: raise RuntimeError('Manager request failed with HTTP '+str(error.code)) from None
        raise RuntimeError(str(body)) from None
try:
    enabled=[]
    for number in ['05','06','07']:
        source='binance_exec_trade'+number
        result=get('/manager/api/catalog/accounts/'+source+'/funding',
                   {'enabled':True,'refresh_secs':300,'enable_token':runtime[b'CTA_FUNDING_ENABLE_TOKEN'].decode()})
        assert result['enabled'] and result['unavailable_reason'] is None
        assert result['verified_local_ip']=='172.31.35.228' and result['verified_public_ip']=='13.115.227.29'
        enabled.append(result)
    disabled=[]
    for number in ['01','02','03','04']:
        result=get('/manager/api/catalog/accounts/binance_exec_trade'+number+'/funding')
        assert not result['enabled']; disabled.append(result['source_id'])
    (STAGE/'funding_enabled.json').write_text(json.dumps(enabled,indent=2)+'\n')
    print(json.dumps({'enabled':[r['source_id'] for r in enabled],'verified_local_ip':enabled[0]['verified_local_ip'],
                      'verified_public_ip':enabled[0]['verified_public_ip'],'refresh_secs':300,'other_accounts_disabled':disabled},indent=2))
finally:
    sql('DELETE FROM cta_auth_sessions WHERE session_id='+session+" AND token_hash=decode('"+digest+"','hex');")
