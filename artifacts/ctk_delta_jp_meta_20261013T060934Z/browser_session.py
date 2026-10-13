from pathlib import Path
import json,os,sys
ROOT=Path('/home/ubuntu/crypto_cta_manager')
STAGE=ROOT/'deploy-staging/ctk-delta-20261013T060934Z'
mode=sys.argv[1]
# Load only the already-reviewed connection helpers, without running verification.
source=(STAGE/'verify_remote.py').read_text().split('def check_protected():')[0]
exec(source)
env=db_env()
p=STAGE/'browser-session.json'
if mode=='create':
    assert not p.exists()
    token=secrets.token_urlsafe(32);digest=hashlib.sha256(token.encode()).hexdigest()
    row=sql("INSERT INTO cta_auth_sessions (user_id,token_hash,expires_at) SELECT user_id,decode('"+digest+"','hex'),now()+interval '4 hours' FROM cta_users WHERE role='admin' AND disabled=false ORDER BY user_id LIMIT 1 RETURNING session_id;",env)
    assert row.isdigit()
    fd=os.open(p,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)
    with os.fdopen(fd,'w') as f:json.dump({'token':token,'digest':digest,'session_id':row},f)
    print('Temporary browser session created; credentials not printed')
else:
    session=json.loads(p.read_text())
    sql("DELETE FROM cta_auth_sessions WHERE session_id="+session['session_id']+" AND token_hash=decode('"+session['digest']+"','hex');",env)
    assert sql('SELECT count(*) FROM cta_auth_sessions WHERE session_id='+session['session_id']+';',env)=='0'
    p.unlink()
    print('Temporary browser session deleted')
