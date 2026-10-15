"""Check post-release health without exposing an authentication session."""
import hashlib
import json
import os
import pathlib
import secrets
import subprocess
import urllib.parse
import urllib.request

pid = int(subprocess.check_output(['systemctl', '--user', 'show', 'crypto-cta-manager-web.service', '-p', 'MainPID', '--value'], text=True))
runtime = dict(x.split(b'=', 1) for x in pathlib.Path(f'/proc/{pid}/environ').read_bytes().split(b'\0') if b'=' in x)
url = urllib.parse.urlsplit(runtime[b'CRYPTO_CTA_LOCAL_DATABASE_URL'].decode())
env = dict(os.environ, PGHOST=url.hostname, PGPORT=str(url.port or 5432), PGUSER=urllib.parse.unquote(url.username),
           PGPASSWORD=urllib.parse.unquote(url.password or ''), PGDATABASE=url.path.lstrip('/'))


def sql(query):
    result = subprocess.run(['/home/el01/.local/opt/postgresql/16/bin/psql', '-X', '-qAt', '-v', 'ON_ERROR_STOP=1'], input=query, capture_output=True, text=True, env=env)
    assert result.returncode == 0, 'database query failed (details suppressed)'
    return result.stdout.strip()


token = secrets.token_urlsafe(32)
digest = hashlib.sha256(token.encode()).hexdigest()
session = sql("INSERT INTO cta_auth_sessions(user_id,token_hash,expires_at) SELECT user_id,decode('" + digest + "','hex'),now()+interval '5 minutes' FROM cta_users WHERE role='admin' AND NOT disabled ORDER BY user_id LIMIT 1 RETURNING session_id;")
assert session.isdecimal()
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def get(path):
    request = urllib.request.Request('http://127.0.0.1:10051/manager/api/' + path, headers={'Cookie': 'cta_session=' + token})
    with opener.open(request, timeout=30) as response:
        return json.load(response)


try:
    state = {'manager_pid': pid, 'health': get('health'), 'target_history': get('catalog/kline-status')['target_history'], 'funding': []}
    for number in ('09',):
        source = 'binance_exec_trade' + number
        setting = get('catalog/accounts/' + source + '/funding')
        state['funding'].append(dict(source_id=source, enabled=setting['enabled'],
                                    status=setting['summary']['status'], records=setting['summary']['record_count'],
                                    has_error=setting['summary']['last_error'] is not None))
    assert state['health']['status'] == 'ok' and state['health']['last_refresh_error'] is None
    assert all(row['enabled'] and not row['has_error'] for row in state['funding'])
    assert state['target_history']['last_error'] is None
finally:
    sql('DELETE FROM cta_auth_sessions WHERE session_id=' + session + " AND token_hash=decode('" + digest + "','hex');")
    assert sql("SELECT count(*) FROM cta_auth_sessions WHERE token_hash=decode('" + digest + "','hex');") == '0'
state['temporary_session_removed'] = True
print(json.dumps(state, indent=2))
