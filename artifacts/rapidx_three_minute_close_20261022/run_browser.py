import json
from pathlib import Path
import subprocess
import time
import urllib.request
import urllib.error
p=Path(__file__).parent
header=(p.parent/'rapidx_three_minute_20261022/remote_common.py').read_text()
create=header+"""
token=secrets.token_urlsafe(32)
digest=hashlib.sha256(token.encode()).hexdigest()
session_id=sql("INSERT INTO cta_auth_sessions (user_id,token_hash,expires_at) SELECT user_id,decode('"+digest+"','hex'),now()+interval '30 minutes' FROM cta_users WHERE role='admin' AND disabled=false ORDER BY user_id LIMIT 1 RETURNING session_id;",readonly=False)
print(json.dumps({'token':token,'session_id':session_id}))
"""
remote=['ssh','-o','BatchMode=yes','jp-meta-elvpn','python3','-']
r=subprocess.run(remote,input=create,text=True,capture_output=True,check=True)
session=json.loads(r.stdout);assert session['session_id'].isdigit()
try:
    opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
    began=time.monotonic()
    while True:
        try:
            with opener.open(urllib.request.Request('http://13.115.227.29:4191/manager/api/health',headers={'Cookie':'cta_session='+session['token']}),timeout=20) as response:
                assert json.load(response)['status']=='ok'
            break
        except urllib.error.HTTPError as error:
            if error.code not in [502,503,504] or time.monotonic()-began>360:raise
            time.sleep(5)
    result=subprocess.run(['ssh','-o','BatchMode=yes','el_dev','node','/home/fanghaizhou/snap/chromium/common/rapidx-three-minute-close-20261022/browser_verify.mjs','/home/fanghaizhou/snap/chromium/common/rapidx-three-minute-close-20261022'],input=json.dumps(session),text=True,capture_output=True)
    (p/'browser_result.json').write_text(result.stdout)
    if result.returncode:raise RuntimeError(result.stderr)
    parsed=json.loads(result.stdout);print(json.dumps({'passed':parsed['passed'],'viewports':len(parsed['results'])}))
finally:
    cleanup=header+'\nprint(sql("WITH removed AS (DELETE FROM cta_auth_sessions WHERE session_id='+session['session_id']+' RETURNING session_id) SELECT count(*) FROM removed;",readonly=False))\n'
    r=subprocess.run(remote,input=cleanup,text=True,capture_output=True,check=True)
    assert r.stdout.strip()=='1'
    print(json.dumps({'verification_session_removed':True}))
