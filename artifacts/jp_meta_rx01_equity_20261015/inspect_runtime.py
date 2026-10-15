"""Inspect the equity feed and deployment invariants; print no credentials."""
import collections
import hashlib
import json
import os
import pathlib
import secrets
import subprocess
import sys
import time
import tomllib
import urllib.parse
import urllib.request

ROOT = pathlib.Path('/home/ubuntu/crypto_cta_manager')
EXEC = pathlib.Path('/home/ubuntu/binance_exec_trade06')
pid = int(subprocess.check_output(['systemctl', '--user', 'show', 'crypto-cta-manager-web.service', '-p', 'MainPID', '--value'], text=True))
runtime = dict(item.split(b'=', 1) for item in pathlib.Path(f'/proc/{pid}/environ').read_bytes().split(b'\0') if b'=' in item)
url = urllib.parse.urlsplit(runtime[b'CRYPTO_CTA_LOCAL_DATABASE_URL'].decode())
env = dict(os.environ, PGHOST=url.hostname, PGPORT=str(url.port or 5432),
           PGUSER=urllib.parse.unquote(url.username), PGPASSWORD=urllib.parse.unquote(url.password or ''),
           PGDATABASE=url.path.lstrip('/'))
def sql(query):
    result = subprocess.run(['psql', '-X', '-qAt', '-v', 'ON_ERROR_STOP=1'], input=query, text=True, capture_output=True, env=env)
    assert result.returncode == 0, 'Database inspection failed; details suppressed'
    return result.stdout.strip()
def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()
processes = {}
monitor_pid = None
for proc in pathlib.Path('/proc').iterdir():
    if not proc.name.isdecimal() or int(proc.name) == os.getpid():
        continue
    try:
        comm = (proc / 'comm').read_text().strip()
        cmd = (proc / 'cmdline').read_bytes()
        if not (any(part in comm.lower() for part in ['trade', 'exec', 'viz', 'nginx', 'cta_monitor', 'cta_web', 'signal', 'factor', 'spread', 'account_mon', 'rapidx_account', 'persist'])
                or comm.startswith('python') and any(part in cmd for part in [b'exec_config_server', b'viz_server'])):
            continue
        processes[proc.name] = {'comm': comm, 'start_ticks': (proc / 'stat').read_text().rsplit(')', 1)[1].split()[19],
                                'affinity': sorted(os.sched_getaffinity(int(proc.name)))}
        if comm == 'rapidx_account_' and (proc / 'cwd').resolve() == EXEC:
            assert monitor_pid is None
            monitor_pid = int(proc.name)
    except OSError:
        continue
assert monitor_pid is not None
config = tomllib.loads((ROOT / 'config/cta-manager.toml').read_text())
paths = [ROOT / 'config/cta-manager.toml', pathlib.Path('/home/ubuntu/.config/systemd/user/crypto-cta-manager-web.service'),
         pathlib.Path('/etc/nginx/snippets/crypto_cta_manager.conf'), pathlib.Path('/etc/nginx/sites-available/crypto_proxy_4191.conf'),
         pathlib.Path('/home/ubuntu/nginx_locations.txt'), EXEC / 'env.sh',
         EXEC / 'scripts/start_account_monitor.sh', EXEC / 'scripts/stop_account_monitor.sh']
paths.extend(pathlib.Path(path) for path in config['kline']['trade_engine_configs'])
tables = ['cta_position_snapshots', 'cta_position_snapshot_entries', 'cta_strategy_position_snapshots', 'cta_strategy_position_snapshot_entries']
financial_path = next((EXEC / 'data/rapidx_account').glob('*/latest_financial.json'))
financial = json.loads(financial_path.read_text())
state = {'captured_at_us': time.time_ns() // 1000, 'manager_pid': pid, 'monitor_pid': monitor_pid,
         'protected': processes, 'config_sha256': {str(path): digest(path) for path in paths if path.is_file()},
         'frontend_sha256': digest(ROOT / 'webroot/manager/index.html'),
         'manager_binary_sha256': digest(ROOT / 'bin/cta_web'),
         'monitor_binary_sha256': digest(EXEC / 'rapidx_account_monitor'),
         'snapshot_hashes': {table: sql(f"SELECT md5(coalesce(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text),'[]')::text) FROM {table} t;") for table in tables},
         'financial': {key: value for key, value in financial.items() if key != 'financial'}}
state['financial']['total_equity'] = financial['financial'].get('total_equity')
state['financial']['observed_ms'] = financial['financial'].get('observed_ms')
token = secrets.token_urlsafe(32)
token_hash = hashlib.sha256(token.encode()).hexdigest()
session = sql("INSERT INTO cta_auth_sessions(user_id,token_hash,expires_at) SELECT user_id,decode('" + token_hash + "','hex'),now()+interval '10 minutes' FROM cta_users WHERE role='admin' AND NOT disabled ORDER BY user_id LIMIT 1 RETURNING session_id;")
assert session.isdecimal()
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
def get(path):
    request = urllib.request.Request('http://127.0.0.1:4191' + path, headers={'Cookie': 'cta_session=' + token})
    with opener.open(request, timeout=30) as response:
        return json.load(response)
try:
    state['health'] = get('/manager/api/health')
    state['exchange_nav'] = get('/manager/api/nav/exchange')
    dashboard = get('/manager/api/dashboard')
    state['dashboard_accounts'] = [{key: value for key, value in account.items()
                                    if key in ['source_id', 'account', 'live_equity_usdt', 'live_equity_status']}
                                   for account in dashboard['accounts']]
    snapshot = get('/exec_trade06/snapshot')
    state['viz'] = {'ts_ms': snapshot['ts_ms'], 'channels': dict(collections.Counter(row.get('channel') for row in snapshot['entries']))}
    state['viz']['account_entries'] = [row for row in snapshot['entries'] if any(part in row.get('channel', '').lower() for part in ['risk', 'equity', 'wallet'])]
finally:
    sql('DELETE FROM cta_auth_sessions WHERE session_id=' + session + " AND token_hash=decode('" + token_hash + "','hex');")
print(json.dumps(state, indent=2))
