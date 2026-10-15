"""Keep diagnostic cookies in memory and delete sessions in finally."""
from pathlib import Path
import json
import subprocess

artifact = Path(__file__).resolve().parent
common = (artifact / 'common_remote.py').read_text()
create = common + '''
env=database_env()
sessions={}
for username in ['shaokai','admin']:
    token=secrets.token_urlsafe(32)
    digest=hashlib.sha256(token.encode()).hexdigest()
    where="role='admin'" if username=='admin' else "username='shaokai'"
    session_id=sql("INSERT INTO cta_auth_sessions (user_id,token_hash,expires_at) SELECT user_id,decode('"+digest+"','hex'),now()+interval '10 minutes' FROM cta_users WHERE disabled=false AND "+where+" ORDER BY user_id LIMIT 1 RETURNING session_id;",env)
    assert session_id.isdigit()
    sessions[username]={'token':token,'session_id':session_id}
print(json.dumps(sessions))
'''
created = subprocess.run(['ssh', 'cta_exec', 'python3', '-'], input=create,
                         text=True, capture_output=True, check=True)
sessions = json.loads(created.stdout)
stage = '/home/fanghaizhou/snap/chromium/common/cta-owner-check-20261015'
try:
    result = subprocess.run(['ssh', 'el_dev', 'node', stage + '/browser_check.mjs', stage],
                            input=json.dumps(sessions), text=True, capture_output=True)
    (artifact / 'browser_result.json').write_text(result.stdout)
    if result.returncode:
        raise RuntimeError(result.stderr)
    parsed = json.loads(result.stdout)
    print(json.dumps({'passed': parsed['passed'], 'scenarios': len(parsed['results'])}))
finally:
    ids = ','.join(str(int(session['session_id'])) for session in sessions.values())
    cleanup = common + '\nenv=database_env()\nprint(sql("WITH removed AS (DELETE FROM cta_auth_sessions WHERE session_id IN (' + ids + ') RETURNING session_id) SELECT count(*) FROM removed;",env))\n'
    removed = subprocess.run(['ssh', 'cta_exec', 'python3', '-'], input=cleanup,
                            text=True, capture_output=True, check=True)
    assert removed.stdout.strip() == str(len(sessions))
    print(json.dumps({'temporary_browser_sessions_removed': len(sessions)}))
