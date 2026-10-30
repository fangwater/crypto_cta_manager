"""Scoped Manager recovery. Exchange credentials remain on this host in memory."""
import contextlib
import hashlib
import json
import os
from pathlib import Path
import secrets
import shutil
import subprocess
import sys
import time
import tomllib
import urllib.error
import urllib.request

ROOT = Path('/home/ubuntu/crypto_cta_manager')
WORK = ROOT / 'backups/bnb-self-heal-20261030'
UNIT = 'crypto-cta-manager-web.service'
SOURCES = ['binance_exec_trade03', 'binance_exec_trade04']
cfg = tomllib.loads((ROOT / 'config/cta-manager.toml').read_text())
assert subprocess.check_output(['whoami'], text=True).strip() == 'ubuntu'
assert subprocess.check_output(['hostname', '-f'], text=True).strip() == 'ip-172-31-35-228.ap-northeast-1.compute.internal'


def emit(name, body):
    print(json.dumps({'name': name, 'body': body}, ensure_ascii=False), flush=True)


def sql(query):
    p = subprocess.run(['sudo', '-n', '-u', 'postgres', 'psql', '-X', '-qAt',
                        '-v', 'ON_ERROR_STOP=1', '-d', 'crypto_cta_manager'],
                       input=query, text=True, capture_output=True)
    if p.returncode:
        raise RuntimeError('Scoped Manager database operation failed; details suppressed')
    return p.stdout.strip()


def save(name, body):
    path = WORK / name
    path.write_text(json.dumps(body, indent=2, sort_keys=True) + '\n')
    path.chmod(0o600)


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def protected():
    result = dict(processes={}, config_hashes={}, snapshots={})
    for p in Path('/proc').iterdir():
        if not p.name.isdigit():
            continue
        try:
            exe = str((p / 'exe').resolve())
            if not any(n in Path(exe).name for n in ['trade_engine', 'exec-pre-trade',
                    'persist_manager', 'exec_config', 'viz_server', 'account_monitor',
                    'nginx', 'cta_monitor', 'spread_pbs', 'redis-server', 'cta_perp', 'cta_spread']):
                continue
            result['processes'][p.name] = dict(exe=exe,
                start_ticks=(p / 'stat').read_text().rsplit(')', 1)[1].split()[19])
        except (OSError, IndexError):
            pass
    paths = [ROOT / 'config/cta-manager.toml', Path('/etc/nginx/snippets/crypto_cta_manager.conf'),
             ROOT / 'crypto-cta-manager-web.service']
    paths.extend(Path(p) for p in cfg.get('kline', {}).get('trade_engine_configs', []))
    paths.extend(Path(s['rocksdb_path']).parent.parent / 'trade_engine.toml' for s in cfg['sources'])
    for path in paths:
        if path.is_file():
            result['config_hashes'][str(path)] = sha(path)
    for table in ['cta_position_snapshots', 'cta_position_snapshot_entries',
                  'cta_strategy_position_snapshots', 'cta_strategy_position_snapshot_entries',
                  'cta_strategy_allocation_snapshots']:
        result['snapshots'][table] = sql("BEGIN READ ONLY; SELECT md5(coalesce(string_agg("
            "row_to_json(t)::text,E'\\n' ORDER BY row_to_json(t)::text),'')) FROM " + table + " t; COMMIT;")
    result['bnb_settings'] = {s: a['settings'] for s, a in
        json.loads((ROOT / 'config/bnb-auto.json').read_text())['accounts'].items()}
    result['bfusd_settings_hash'] = sha(ROOT / 'config/bfusd-auto.json')
    result['webroot'] = str((ROOT / 'webroot').resolve())
    return result


@contextlib.contextmanager
def gateway():
    token = secrets.token_urlsafe(32)
    digest = hashlib.sha256(token.encode()).hexdigest()
    session = sql("INSERT INTO cta_auth_sessions(user_id,token_hash,expires_at) "
        "SELECT user_id,decode('" + digest + "','hex'),now()+interval '15 minutes' "
        "FROM cta_users WHERE role='admin' AND NOT disabled ORDER BY user_id LIMIT 1 RETURNING session_id;")
    assert session.isdigit()
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    def api(path, body=None):
        request = urllib.request.Request('http://127.0.0.1:4191/manager/api/' + path,
            data=None if body is None else json.dumps(body).encode(),
            headers={'Cookie': 'cta_session=' + token, 'Content-Type': 'application/json'})
        try:
            with opener.open(request, timeout=55) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            try:
                body = json.loads(error.read())
            except ValueError:
                body = 'Gateway unavailable'
            return dict(http=error.code, error=body)
    try:
        yield api
    finally:
        sql('DELETE FROM cta_auth_sessions WHERE session_id=' + session + ';')
        emit('diagnostic_session_removed', True)


mode = sys.argv[1]
if mode == 'backup':
    assert not WORK.exists(), 'Backup directory already exists'
    WORK.mkdir(mode=0o700)
    for name in ['bnb-auto.json', 'bfusd-auto.json']:
        shutil.copy2(ROOT / 'config' / name, WORK / (name + '.before'))
    os.link(ROOT / 'bin/cta_web', WORK / 'cta_web.before')
    save('protected.before.json', protected())
    save('binary.before.json', dict(sha256=sha(ROOT / 'bin/cta_web')))
    emit('backup', str(WORK))
elif mode == 'deploy':
    stage = ROOT / 'releases/bnb-self-heal-20261030'
    manifest = json.loads((stage / 'manifest.json').read_text())
    assert sha(stage / 'cta_web') == manifest['sha256']
    assert sha(ROOT / 'bin/cta_web') == json.loads((WORK / 'binary.before.json').read_text())['sha256']
    assert protected() == json.loads((WORK / 'protected.before.json').read_text()), 'Protected state changed before deployment'
    os.chmod(stage / 'cta_web', 0o755)
    os.replace(stage / 'cta_web', ROOT / 'bin/cta_web')
    subprocess.run(['systemctl', '--user', 'restart', UNIT], check=True)
    subprocess.run(['systemctl', '--user', '--quiet', 'is-active', UNIT], check=True)
    save('deployment.json', manifest)
    emit('deployed', manifest)
elif mode == 'verify':
    before = json.loads((WORK / 'protected.before.json').read_text())
    after = protected()
    save('protected.after.json', after)
    checks = {k: before[k] == after[k] for k in before}
    emit('protected_checks', checks)
    emit('protected_counts', {k: len(after[k]) for k in ['processes', 'config_hashes', 'snapshots']})
    assert all(checks.values())
elif mode in ['status', 'run']:
    with gateway() as api:
        if mode == 'run':
            # Use only Manager's journaled controller with unchanged saved settings.
            emit('trade03_run', api('catalog/accounts/binance_exec_trade03/bnb-auto/run', {}))
        result = {}
        for source in SOURCES:
            status = api('catalog/accounts/' + source + '/bnb-auto')
            result[source] = status
            emit(source, {k: v for k, v in status.items() if k != 'audit'})
            emit(source + '_audit_tail', status.get('audit', [])[-7:])
            emit(source + '_bfusd', api('catalog/accounts/' + source + '/bfusd-auto'))
        save(mode + '_' + str(time.time_ns()) + '.json', result)
        emit('health', api('health'))
        emit('manager_service', subprocess.check_output(['systemctl', '--user', 'show', UNIT,
            '--property=MainPID,ActiveState,CPUAffinity,ExecMainStartTimestamp'], text=True))
else:
    raise SystemExit('Unknown operation')
