"""Use an ephemeral lgq session over stdin and delete it in finally."""
import json
import pathlib
import subprocess

root = pathlib.Path(__file__).resolve().parent
source = (root / 'inspect_runtime.py').read_text()
header = source[:source.index('\ndef digest')]
create = header + '''
token = secrets.token_urlsafe(32)
token_hash = hashlib.sha256(token.encode()).hexdigest()
session = sql("INSERT INTO cta_auth_sessions(user_id,token_hash,expires_at) SELECT user_id,decode('" + token_hash + "','hex'),now()+interval '10 minutes' FROM cta_users WHERE username='lgq' AND NOT disabled RETURNING session_id;")
assert session.isdecimal()
print(json.dumps({'token': token, 'session_id': session, 'token_hash': token_hash}))
'''
created = subprocess.run(['ssh', 'jp-meta', 'python3', '-'], input=create, text=True, capture_output=True, check=True)
session = json.loads(created.stdout)
assert session['session_id'].isdecimal()
try:
    result = subprocess.run(['ssh', 'el_dev', 'node', '/home/fanghaizhou/snap/chromium/common/rx01-equity-20261015/browser_check.mjs'],
                            input=json.dumps({'token': session['token']}), text=True, capture_output=True, timeout=90)
    if result.returncode:
        raise RuntimeError(result.stderr)
    parsed = json.loads(result.stdout)
    assert parsed['passed']
    (root / 'browser_result.json').write_text(json.dumps(parsed, indent=2) + '\n')
    print(json.dumps(parsed))
finally:
    cleanup = header + '\nprint(sql("WITH removed AS (DELETE FROM cta_auth_sessions WHERE session_id=' + session['session_id'] + " AND token_hash=decode('" + session['token_hash'] + "','hex') RETURNING session_id) SELECT count(*) FROM removed;\"))\n"
    removed = subprocess.run(['ssh', 'jp-meta', 'python3', '-'], input=cleanup, text=True, capture_output=True, check=True)
    assert removed.stdout.strip() == '1'
    print('ephemeral lgq session removed')
