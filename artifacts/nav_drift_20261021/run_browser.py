import json
from pathlib import Path
import subprocess
p=Path(__file__).parent
source=(p/'extra_remote.py').read_text()
header=source[:source.index('\ntoken = secrets.token_urlsafe')]
create=source[:source.index('\nopener = urllib.request.build_opener')]+"\nprint(json.dumps({'token':token,'session_id':session_id}))\n"
remote=['ssh','-o','BatchMode=yes','el_dev','set -a; . ~/.config/crypto-cta-manager/gateway-tunnel.env; set +a; ssh -o BatchMode=yes "$CTA_EXEC_SSH_TARGET" "python3 -"']
r=subprocess.run(remote,input=create,text=True,capture_output=True,check=True)
session=json.loads(r.stdout);assert session['session_id'].isdigit()
try:
 result=subprocess.run(['ssh','-o','BatchMode=yes','el_dev','node','/home/fanghaizhou/snap/chromium/common/nav-marks-20261021/browser_verify.mjs','/home/fanghaizhou/snap/chromium/common/nav-marks-20261021'],input=json.dumps(session),text=True,capture_output=True)
 (p/'browser_result.json').write_text(result.stdout)
 if result.returncode:raise RuntimeError(result.stderr)
 parsed=json.loads(result.stdout);print(json.dumps({'passed':parsed['passed'],'viewports':len(parsed['results'])}))
finally:
 cleanup=header+'\nprint(sql("WITH removed AS (DELETE FROM cta_auth_sessions WHERE session_id='+session['session_id']+' RETURNING session_id) SELECT count(*) FROM removed;"))\n'
 r=subprocess.run(remote,input=cleanup,text=True,capture_output=True,check=True)
 assert r.stdout.strip()=='1'
 print(json.dumps({'verification_session_removed':True}))
