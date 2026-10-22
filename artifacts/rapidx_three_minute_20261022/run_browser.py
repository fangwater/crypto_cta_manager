import json
from pathlib import Path
import subprocess
p=Path(__file__).parent
header=(p/'remote_common.py').read_text()
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
    result=subprocess.run(['ssh','-o','BatchMode=yes','el_dev','node','/home/fanghaizhou/snap/chromium/common/rapidx-three-minute-20261022/browser_verify.mjs','/home/fanghaizhou/snap/chromium/common/rapidx-three-minute-20261022'],input=json.dumps(session),text=True,capture_output=True)
    (p/'browser_result.json').write_text(result.stdout)
    if result.returncode:raise RuntimeError(result.stderr)
    parsed=json.loads(result.stdout);print(json.dumps({'passed':parsed['passed'],'viewports':len(parsed['results'])}))
finally:
    cleanup=header+'\nprint(sql("WITH removed AS (DELETE FROM cta_auth_sessions WHERE session_id='+session['session_id']+' RETURNING session_id) SELECT count(*) FROM removed;",readonly=False))\n'
    r=subprocess.run(remote,input=cleanup,text=True,capture_output=True,check=True)
    assert r.stdout.strip()=='1'
    print(json.dumps({'verification_session_removed':True}))
