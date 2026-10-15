"""Capture scoped Manager anchors and stable read-only Exec history via SSH."""
import hashlib
import json
import os
import pathlib
import secrets
import subprocess
import time
import tomllib
import urllib.parse
import urllib.request

ROOT = pathlib.Path('/home/ubuntu/crypto_cta_manager')
STAGE = ROOT / 'backups/rx01_initial_snapshot_20261015'
SOURCE = 'binance_exec_trade06'
TABLES = ['cta_position_snapshots', 'cta_position_snapshot_entries',
          'cta_strategy_position_snapshots', 'cta_strategy_position_snapshot_entries']
CONFIG = tomllib.loads((ROOT / 'config/cta-manager.toml').read_text())
source = next(s for s in CONFIG['sources'] if s['id'] == SOURCE)
assert source['alias'] == 'lgq_cta_RX01' and source['enabled']
pid = int(subprocess.check_output(['systemctl', '--user', 'show',
          'crypto-cta-manager-web.service', '-p', 'MainPID', '--value'], text=True))
runtime = dict(x.split(b'=', 1) for x in pathlib.Path(f'/proc/{pid}/environ').read_bytes().split(b'\0') if b'=' in x)
url = urllib.parse.urlsplit(runtime[CONFIG['database']['url_env'].encode()].decode())
env = dict(os.environ)
env.update(PGHOST=url.hostname, PGPORT=str(url.port or 5432), PGUSER=urllib.parse.unquote(url.username),
           PGPASSWORD=urllib.parse.unquote(url.password or ''), PGDATABASE=urllib.parse.unquote(url.path.lstrip('/')),
           PGCONNECT_TIMEOUT='10')


def sql(query):
    r = subprocess.run(['psql', '-X', '-qAt', '-v', 'ON_ERROR_STOP=1'], input=query,
                       capture_output=True, text=True, env=env, timeout=30)
    if r.returncode:
        raise RuntimeError('database operation failed; SQL and credential values suppressed')
    return r.stdout.strip()


def save(name, value):
    (STAGE / name).write_text(json.dumps(value, indent=2, ensure_ascii=False) + '\n')


anchors = {table: json.loads(sql(f"SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text),'[]') FROM {table} t WHERE source_id='{SOURCE}';")) for table in TABLES}
if not (STAGE / 'original_anchors.json').exists():
    save('original_anchors.json', anchors)
    with (STAGE / 'snapshots_before.sql').open('w') as out:
        r = subprocess.run(['pg_dump', '--data-only', '--no-owner', '--no-privileges',
                            *[x for t in TABLES for x in ['--table', t]]], env=env, stdout=out,
                           stderr=subprocess.PIPE, text=True, timeout=30)
    assert r.returncode == 0, 'snapshot backup failed'
    processes = {}
    for proc in pathlib.Path('/proc').iterdir():
        if not proc.name.isdecimal():
            continue
        try:
            comm = (proc / 'comm').read_text().strip()
            if any(x in comm.lower() for x in ['trade', 'exec', 'viz', 'nginx', 'persist', 'cta_web']):
                processes[proc.name] = {'comm': comm, 'start_ticks': (proc / 'stat').read_text().rsplit(')', 1)[1].split()[19]}
        except OSError:
            pass
    save('protected_processes_before.json', processes)
    save('other_source_anchors_before.json', {table: sql(f"SELECT md5(coalesce(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text),'[]')::text) FROM {table} t WHERE source_id<>'{SOURCE}';") for table in TABLES})

token = secrets.token_urlsafe(32)
digest = hashlib.sha256(token.encode()).hexdigest()
session_id = sql("INSERT INTO cta_auth_sessions (user_id,token_hash,expires_at) SELECT user_id,decode('" + digest + "','hex'),now()+interval '15 minutes' FROM cta_users WHERE role='admin' AND disabled=false ORDER BY user_id LIMIT 1 RETURNING session_id;")
assert session_id.isdecimal(), 'no available administrator'
try:
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    def get(path):
        req = urllib.request.Request('http://127.0.0.1:4191' + path, headers={'Cookie': 'cta_session=' + token})
        with opener.open(req, timeout=175) as response:
            return json.load(response)

    for attempt in range(5):
        first = subprocess.check_output([str(STAGE / 'export_history'), source['rocksdb_path']])
        snapshot = get(source['gateway_prefix'] + '/snapshot')
        second = subprocess.check_output([str(STAGE / 'export_history'), source['rocksdb_path']])
        if first == second:
            break
    else:
        raise RuntimeError('history changed throughout stable snapshot attempts')
    (STAGE / 'uniform_orders.jsonl').write_bytes(second)
    save('exec_snapshot.json', snapshot)
    save('dashboard_before.json', get('/manager/api/dashboard'))
    save('health_before.json', get('/manager/api/health'))
    events = [json.loads(line) for line in second.splitlines()]
    state = next(x['entry'] for x in snapshot['entries'] if x.get('type') == 'exec_pre_trade_state')
    assert state['position_ready']
    assert abs(time.time() * 1000 - state['ts_ms']) < 30000
    account = {}
    for row in state['rows']:
        if row['symbol'] in account:
            assert account[row['symbol']] == row['account_position_qty']
        account[row['symbol']] = row['account_position_qty']
    anchor = max(x['snapshot_ts_us'] for x in anchors['cta_position_snapshots'])
    net = {}
    fills = [e for e in events if e['amount_update'] > 0 and (e['update_ts_us'] if e['update_ts_us'] > 0 else e['event_ts_us']) > anchor]
    for e in fills:
        assert e['venue_code'] in [0, 1] and e['side_code'] in [1, 2]
        symbol = e['symbol']
        if e['venue_code'] == 0 and symbol.endswith('USDT'):
            symbol = symbol[:-4]
        net[symbol] = net.get(symbol, 0) + e['amount_update'] * (1 if e['side_code'] == 1 else -1)
    old = {r['symbol']: r['quantity'] for r in anchors['cta_position_snapshot_entries'] if r['snapshot_ts_us'] == anchor}
    differences = []
    for symbol in sorted(set(account) | set(net) | set(old)):
        inferred = account.get(symbol, 0) - net.get(symbol, 0)
        diff = inferred - old.get(symbol, 0)
        if abs(diff) > 1e-9:
            differences.append({'symbol': symbol, 'old': old.get(symbol, 0), 'current': account.get(symbol, 0),
                                'signed_fills': net.get(symbol, 0), 'inferred_initial': inferred, 'difference': diff})
    summary = {'anchor_ts_us': anchor, 'snapshot_ts_ms': state['ts_ms'], 'events': len(events),
               'fills': len(fills), 'unpriced_fills': sum(e['price'] <= 0 for e in fills),
               'last_key': events[-1]['record_key'], 'differences': differences,
               'account_positions': account}
    save('inference_summary.json', summary)
    print(json.dumps(summary, ensure_ascii=False, indent=2))
finally:
    sql('DELETE FROM cta_auth_sessions WHERE session_id=' + session_id + " AND token_hash=decode('" + digest + "','hex');")
