"""Host-local checks; database credentials and temporary cookies stay in memory."""
import contextlib
import hashlib
import json
import os
from pathlib import Path
import secrets
import subprocess
import time
import tomllib
import urllib.error
import urllib.parse
import urllib.request

ROOT = Path('/home/ubuntu/crypto_cta_manager')
UNIT = 'crypto-cta-manager-web.service'
cfg = tomllib.loads((ROOT / 'config/cta-manager.toml').read_text())
pid = subprocess.check_output(['systemctl', '--user', 'show', UNIT, '-p', 'MainPID', '--value'], text=True).strip()
runtime = dict(x.split(b'=', 1) for x in Path('/proc/' + pid + '/environ').read_bytes().split(b'\0') if b'=' in x)
u = urllib.parse.urlsplit(runtime[cfg['database'].get('url_env', 'CRYPTO_CTA_LOCAL_DATABASE_URL').encode()].decode())
db_env = dict(os.environ, PGHOST=u.hostname, PGPORT=str(u.port or 5432), PGUSER=urllib.parse.unquote(u.username),
              PGPASSWORD=urllib.parse.unquote(u.password or ''), PGDATABASE=urllib.parse.unquote(u.path.lstrip('/')),
              PGOPTIONS='-c statement_timeout=30000', PGCONNECT_TIMEOUT='10')
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def sql(query, readonly=True):
    env = dict(db_env)
    if readonly:
        env['PGOPTIONS'] += ' -c default_transaction_read_only=on'
    r = subprocess.run(['psql', '-X', '-qAt', '-v', 'ON_ERROR_STOP=1'], input=query, env=env, text=True, capture_output=True)
    if r.returncode:
        raise RuntimeError('database operation failed; credentials suppressed')
    return r.stdout.strip()


def emit(name, body):
    print(json.dumps({'name': name, 'body': body}, ensure_ascii=False), flush=True)


def protected():
    current_pid = subprocess.check_output(['systemctl', '--user', 'show', UNIT, '-p', 'MainPID', '--value'], text=True).strip()
    processes = {}
    names = ['trade_engine', 'exec-pre-trade', 'persist_manager', 'exec_config_server', 'viz_server',
             'account_monitor', 'nginx', 'cta_monitor', 'spread_pbs', 'redis-server', 'pre_trade',
             'trade_signal', 'fr_signal_dashboard', 'rolling_metrics', 'delist_risk', 'depth_pub',
             'binance_futures_ipc_proxy', 'spread_bbo_zmq_pub', 'cta_perp', 'cta_spread']
    for p in Path('/proc').iterdir():
        if not p.name.isdigit():
            continue
        try:
            exe = str((p / 'exe').resolve())
            if not any(name in Path(exe).name for name in names):
                continue
            processes[p.name] = {'exe': exe, 'start': (p / 'stat').read_text().rsplit(')', 1)[1].split()[19],
                                 'affinity': next(x for x in (p / 'status').read_text().splitlines() if x.startswith('Cpus_allowed_list:'))}
        except (OSError, IndexError):
            pass
    files = [ROOT / 'config/cta-manager.toml', Path('/etc/nginx/snippets/crypto_cta_manager.conf'),
             Path('/etc/nginx/sites-available/crypto_proxy_4191.conf'), Path('/home/ubuntu/nginx_locations.txt'),
             Path('/home/ubuntu/.config/systemd/user/crypto-cta-manager-web.service')]
    files += [Path(p) for p in cfg.get('kline', {}).get('trade_engine_configs', [])]
    for source in cfg['sources']:
        files.append(Path(source['rocksdb_path']).parent.parent / 'trade_engine.toml')
    hashes = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in files if p.is_file()}
    tables = {}
    for name in ['cta_position_snapshots', 'cta_position_snapshot_entries', 'cta_strategy_position_snapshots',
                 'cta_strategy_position_snapshot_entries', 'cta_strategy_allocation_snapshots', 'cta_account_equity_daily',
                 'cta_order_strategies', 'cta_account_strategy_bindings', 'cta_account_follows', 'cta_virtual_account_bindings']:
        if sql("SELECT to_regclass('" + name + "') IS NOT NULL;") != 't':
            continue
        value = 'to_jsonb(t)'
        if name == 'cta_order_strategies':
            value += "-'grid'-'created_by_user_id'"
        tables[name] = json.loads(sql("SELECT json_build_object('count',count(*),'hash',md5(coalesce(string_agg((" + value + ")::text,E'\\n' ORDER BY (" + value + ")::text),''))) FROM " + name + ' t;'))
    return {'processes': processes, 'config_hashes': hashes, 'tables': tables,
            'binary_sha256': hashlib.sha256((ROOT / 'bin/cta_web').read_bytes()).hexdigest(),
            'webroot': str((ROOT / 'webroot').resolve()),
            'manager_affinity': next(x for x in Path('/proc/' + current_pid + '/status').read_text().splitlines() if x.startswith('Cpus_allowed_list:'))}


@contextlib.contextmanager
def session(username):
    token = secrets.token_urlsafe(32)
    token_hash = hashlib.sha256(token.encode()).hexdigest()
    assert username in ['prc', 'lgq']
    sid = sql("INSERT INTO cta_auth_sessions (user_id,token_hash,expires_at) SELECT user_id,decode('" + token_hash + "','hex'),now()+interval '30 minutes' FROM cta_users WHERE username='" + username + "' AND NOT disabled RETURNING session_id;", False)
    assert sid.isdigit()

    def get(path, method='GET', body=None):
        data = None if body is None else json.dumps(body).encode()
        request = urllib.request.Request('http://127.0.0.1:4191/manager/api/' + path, data=data, method=method,
            headers={'Cookie': 'cta_session=' + token, 'Content-Type': 'application/json'})
        with opener.open(request, timeout=50) as r:
            return json.load(r)
    try:
        yield get
    finally:
        sql('DELETE FROM cta_auth_sessions WHERE session_id=' + sid + ';', False)
        assert sql('SELECT count(*) FROM cta_auth_sessions WHERE session_id=' + sid + ';') == '0'
        emit('session_removed', username)


def provider(settings):
    token = runtime[settings['token_env'].encode()].decode()
    req = urllib.request.Request(settings['url'], headers={'Authorization': 'Bearer ' + token})
    with opener.open(req, timeout=20) as r:
        return json.load(r)
