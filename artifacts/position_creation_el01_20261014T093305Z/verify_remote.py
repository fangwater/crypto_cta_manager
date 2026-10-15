"""Verify strategy creation through el01's live Manager gateway.

All verification targets are empty and unbound. Session tokens stay in memory.
No schema, account binding, Exec store, or trading configuration is changed.
"""

from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import secrets
import shutil
import subprocess
import sys
import tomllib
import urllib.error
import urllib.parse
import urllib.request

ROOT = Path('/home/el01/crypto_cta_manager')
RELEASE = '20261014T093305Z'
STAGE = ROOT / 'deploy-staging' / ('position-creation-' + RELEASE)
BACKUP = ROOT / 'backups' / ('position-creation-' + RELEASE)
PG_BIN = Path('/home/el01/.local/opt/postgresql/16/bin')
ORIGIN = 'http://127.0.0.1:10051'
CONFIG = tomllib.loads((ROOT / 'config/cta-manager.toml').read_text())


def manager_pid():
    return int(subprocess.check_output([
        'systemctl', '--user', 'show', 'crypto-cta-manager-web.service',
        '-p', 'MainPID', '--value',
    ], text=True))


def capture_state():
    paths = [ROOT / 'config/cta-manager.toml', ROOT / 'nginx/nginx.conf',
             Path.home() / '.config/systemd/user/crypto-cta-manager-web.service',
             Path.home() / '.config/systemd/user/crypto-cta-nginx.service']
    paths += [Path(p) for p in CONFIG.get('kline', {}).get('trade_engine_configs', [])]
    processes = {}
    for proc in Path('/proc').iterdir():
        if not proc.name.isdigit() or int(proc.name) == os.getpid():
            continue
        try:
            comm = (proc / 'comm').read_text().strip()
            cmd = (proc / 'cmdline').read_bytes().replace(b'\0', b' ').decode(errors='replace')
            if comm == 'cta_web':
                continue
            protected = any(name in comm.lower() for name in [
                'trade_engine', 'persist_manager', 'exec-pre-trade', 'exec_pre_trade',
                'account_monitor', 'trade_signal', 'viz', 'nginx', 'redis-server', 'cta_monitor',
            ]) or (comm.startswith('python') and any(name in cmd for name in [
                'exec_config_server', 'viz_server',
            ])) or (comm == 'postgres' and ' -D ' in cmd)
            if protected:
                stat = (proc / 'stat').read_text().rsplit(')', 1)[1].split()
                processes[proc.name] = {'comm': comm, 'start_ticks': stat[19]}
        except (OSError, IndexError):
            continue
    return {
        'manager_pid': manager_pid(),
        'webroot': str((ROOT / 'webroot').resolve()),
        'protected_processes': processes,
        'config_sha256': {str(p): hashlib.sha256(p.read_bytes()).hexdigest()
                          for p in paths if p.is_file()},
        'trading_config_count': len(CONFIG.get('kline', {}).get('trade_engine_configs', [])),
    }


def database_env():
    runtime = dict(entry.split(b'=', 1) for entry in
                   Path(f'/proc/{manager_pid()}/environ').read_bytes().split(b'\0')
                   if b'=' in entry)
    key = CONFIG.get('database', {}).get('url_env', 'CRYPTO_CTA_LOCAL_DATABASE_URL').encode()
    url = urllib.parse.urlsplit(runtime[key].decode())
    return dict(os.environ, PGHOST=url.hostname, PGPORT=str(url.port or 5432),
                PGUSER=urllib.parse.unquote(url.username),
                PGPASSWORD=urllib.parse.unquote(url.password or ''),
                PGDATABASE=urllib.parse.unquote(url.path.lstrip('/')), PGCONNECT_TIMEOUT='10')


def sql(query, env):
    result = subprocess.run([str(PG_BIN / 'psql'), '-X', '-q', '-A', '-t',
                             '-v', 'ON_ERROR_STOP=1'], input=query, text=True,
                            capture_output=True, env=env)
    if result.returncode:
        raise RuntimeError('verification database operation failed; credentials suppressed')
    return result.stdout.strip()


def prepare():
    STAGE.mkdir(parents=True, exist_ok=True)
    BACKUP.mkdir(mode=0o700, parents=True, exist_ok=False)
    before = capture_state()
    (STAGE / 'before_state.json').write_text(json.dumps(before, indent=2))
    shutil.copy2(ROOT / 'bin/cta_web', BACKUP / 'cta_web')
    if (ROOT / 'RUNTIME-RELEASE.txt').is_file():
        shutil.copy2(ROOT / 'RUNTIME-RELEASE.txt', BACKUP / 'RUNTIME-RELEASE.txt')
    (BACKUP / 'webroot.txt').write_text(os.readlink(ROOT / 'webroot') + '\n')
    env = database_env()
    result = subprocess.run([str(PG_BIN / 'pg_dump'), '-Fc', '-f',
                             str(BACKUP / 'manager.pgdump')], capture_output=True, env=env)
    if result.returncode:
        raise RuntimeError('Manager backup failed; credentials suppressed')
    os.chmod(BACKUP / 'manager.pgdump', 0o600)
    users = sql("SELECT username || '|' || role || '|' || disabled::text FROM cta_users "
                "WHERE username IN ('shaokai','dzy') ORDER BY username;", env)
    assert users.splitlines() == ['dzy|user|false', 'shaokai|user|false']
    print(json.dumps({'phase': 'before', 'manager_pid': before['manager_pid'],
                      'protected_processes': len(before['protected_processes']),
                      'trading_configs': before['trading_config_count'],
                      'ordinary_users': ['shaokai', 'dzy'], 'backup': str(BACKUP)}))


def verify():
    env = database_env()
    sessions = {}
    created_names = []
    fallback_id = None
    prefix = 'permission_check_' + RELEASE + '_'
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    results = {}

    def call(user, path, method='GET', body=None, publish_token=None, cookie=None):
        headers = {}
        if user is not None:
            headers['Cookie'] = 'cta_session=' + sessions[user]['token']
        elif cookie is not None:
            headers['Cookie'] = 'cta_session=' + cookie
        if publish_token is not None:
            headers['X-CTA-Publish-Token'] = publish_token
        data = None
        if body is not None:
            headers['Content-Type'] = 'application/json'
            data = json.dumps(body).encode()
        request = urllib.request.Request(ORIGIN + path, data=data, headers=headers, method=method)
        try:
            with opener.open(request, timeout=60) as response:
                raw = response.read()
                is_json = 'json' in response.headers.get('Content-Type', '')
                return response.status, json.loads(raw) if raw and is_json else None
        except urllib.error.HTTPError as error:
            raw = error.read()
            return error.code, json.loads(raw) if raw else None

    def push(user, name, **kwargs):
        return call(user, '/manager/api/catalog/position-strategies', 'POST',
                    {'strategy_name': name, 'targets': {}}, **kwargs)

    try:
        for username in ['shaokai', 'dzy', 'admin']:
            token = secrets.token_urlsafe(32)
            token_hash = hashlib.sha256(token.encode()).hexdigest()
            where = ("role='admin'" if username == 'admin' else "username='" + username + "'")
            row = sql("INSERT INTO cta_auth_sessions (user_id,token_hash,expires_at) "
                      "SELECT user_id,decode('" + token_hash + "','hex'),now()+interval '15 minutes' "
                      "FROM cta_users WHERE disabled=false AND " + where +
                      " ORDER BY user_id LIMIT 1 RETURNING session_id,user_id;", env)
            session_id, user_id = row.split('|')
            sessions[username] = {'token': token, 'hash': token_hash,
                                  'session_id': int(session_id), 'user_id': int(user_id)}

        new_name = prefix + 'anonymous'
        results['anonymous_create'] = push(None, new_name)[0]
        results['token_only_create'] = push(None, new_name, publish_token='invalid-token')[0]
        results['invalid_session_create'] = push(None, new_name, cookie='invalid-session')[0]
        assert all(results[key] == 403 for key in ['anonymous_create', 'token_only_create', 'invalid_session_create'])
        assert sql("SELECT count(*) FROM cta_position_strategies WHERE strategy_name='" + new_name + "';", env) == '0'

        for username in ['shaokai', 'dzy']:
            name = prefix + username
            assert sql("SELECT count(*) FROM cta_position_strategies WHERE strategy_name='" + name + "';", env) == '0'
            created_names.append(name)
            status, saved = push(username, name)
            results[username + '_create'] = status
            assert status == 200 and saved['publishes'] == []
            owner = sql("SELECT created_by_user_id || '|' || open_visibility::text FROM "
                        "cta_position_strategies WHERE strategy_name='" + name + "';", env)
            assert owner == str(sessions[username]['user_id']) + '|false'
            status, saved = push(username, name)
            results[username + '_owner_update'] = status
            assert status == 200 and saved['publishes'] == []
            other = 'dzy' if username == 'shaokai' else 'shaokai'
            results[other + '_cannot_update_' + username] = push(other, name)[0]
            assert results[other + '_cannot_update_' + username] == 403
            status, catalog = call(username, '/manager/api/catalog/position-strategies')
            assert status == 200
            entry = next(item for item in catalog if item['strategy_name'] == name)
            assert entry['can_publish'] is True
            status, other_catalog = call(other, '/manager/api/catalog/position-strategies')
            assert status == 200 and all(item['strategy_name'] != name for item in other_catalog)
            results[username + '_private_and_editable'] = True

        race_name = prefix + 'race'
        assert sql("SELECT count(*) FROM cta_position_strategies WHERE strategy_name='" + race_name + "';", env) == '0'
        created_names.append(race_name)
        with ThreadPoolExecutor(max_workers=2) as pool:
            futures = {username: pool.submit(push, username, race_name) for username in ['shaokai', 'dzy']}
            statuses = {username: future.result()[0] for username, future in futures.items()}
        assert sorted(statuses.values()) == [200, 403]
        winner = next(username for username, status in statuses.items() if status == 200)
        assert sql("SELECT created_by_user_id FROM cta_position_strategies WHERE strategy_name='" + race_name + "';", env) == str(sessions[winner]['user_id'])
        results['concurrent_same_name_create'] = statuses

        name = prefix + 'shaokai'
        publish_token = secrets.token_urlsafe(32)
        status, _ = call('admin', '/manager/api/catalog/position-strategies/' + name + '/publish-token',
                         'PUT', {'publish_token': publish_token})
        assert status == 200
        results['protected_anonymous_update'] = push(None, name)[0]
        results['protected_wrong_token_update'] = push(None, name, publish_token='invalid-token')[0]
        results['protected_valid_token_update'] = push(None, name, publish_token=publish_token)[0]
        assert [results[k] for k in ['protected_anonymous_update', 'protected_wrong_token_update',
                                    'protected_valid_token_update']] == [403, 403, 200]

        fallback = secrets.token_urlsafe(32)
        status, saved = call('admin', '/manager/api/catalog/publish-tokens', 'POST',
                             {'note': prefix, 'publish_token': fallback})
        assert status == 200
        fallback_id = saved['token_id']
        results['valid_fallback_cannot_create'] = push(None, new_name, publish_token=fallback)[0]
        results['fallback_updates_existing'] = push(None, name, publish_token=fallback)[0]
        assert results['valid_fallback_cannot_create'] == 403
        assert results['fallback_updates_existing'] == 200

        results['health'] = call('shaokai', '/manager/api/health')[0]
        assert results['health'] == 200
        for username in ['shaokai', 'dzy']:
            results[username + '_position_page'] = call(username, '/manager/config/position/')[0]
            assert results[username + '_position_page'] == 200
        if '--browser' in sys.argv:
            # The local orchestrator consumes this line directly from an SSH
            # pipe and passes it to the browser over stdin, without logging it.
            print(json.dumps({'users': {username: {
                'token': sessions[username]['token'], 'own_name': prefix + username,
            } for username in ['shaokai', 'dzy']}}), flush=True)
            browser = json.loads(sys.stdin.readline())
            assert browser.get('ok') is True, 'browser verification failed'
            results['browser'] = browser['checks']
        (STAGE / 'api_verification.json').write_text(json.dumps(results, indent=2))
    finally:
        if fallback_id is not None:
            status, _ = call('admin', '/manager/api/catalog/publish-tokens/' + str(fallback_id), 'DELETE')
            assert status == 204
        for name in created_names:
            safe = sql("SELECT count(*) FROM cta_position_strategies p WHERE strategy_name='" + name +
                       "' AND targets='{}'::jsonb AND created_by_user_id IN (" +
                       ','.join(str(sessions[u]['user_id']) for u in ['shaokai', 'dzy']) +
                       ") AND NOT EXISTS (SELECT 1 FROM cta_account_strategy_bindings b "
                       "WHERE b.position_strategy_name=p.strategy_name);", env)
            if safe == '1':
                status, _ = call('admin', '/manager/api/catalog/position-strategies/' + name, 'DELETE')
                assert status == 204
            assert sql("SELECT count(*) FROM cta_position_strategies WHERE strategy_name='" + name + "';", env) == '0'
        for session in sessions.values():
            sql("DELETE FROM cta_auth_sessions WHERE session_id=" + str(session['session_id']) +
                " AND token_hash=decode('" + session['hash'] + "','hex');", env)
            assert sql("SELECT count(*) FROM cta_auth_sessions WHERE session_id=" + str(session['session_id']) + ';', env) == '0'

    before = json.loads((STAGE / 'before_state.json').read_text())
    after = capture_state()
    assert after['manager_pid'] != before['manager_pid']
    assert after['protected_processes'] == before['protected_processes'], 'protected process changed'
    assert after['config_sha256'] == before['config_sha256'], 'protected configuration changed'
    assert sql("SELECT username || '|' || role || '|' || disabled::text FROM cta_users "
               "WHERE username IN ('shaokai','dzy') ORDER BY username;", env).splitlines() == [
        'dzy|user|false', 'shaokai|user|false']
    (STAGE / 'after_state.json').write_text(json.dumps(after, indent=2))
    results.update(protected_processes_unchanged=len(after['protected_processes']),
                   trading_configs_unchanged=after['trading_config_count'],
                   temporary_sessions_and_catalog_entries_removed=True,
                   user_roles_unchanged=True)
    (STAGE / 'api_verification.json').write_text(json.dumps(results, indent=2))
    print(json.dumps(results), flush=True)


if __name__ == '__main__':
    {'before': prepare, 'after': verify}[sys.argv[1]]()
