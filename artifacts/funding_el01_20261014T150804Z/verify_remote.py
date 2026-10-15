"""Run over SSH on el01; print only sanitized funding deployment evidence."""
import base64
import datetime
import hashlib
import http.client
import json
import os
import pathlib
import socket
import subprocess
import time
import tomllib
import urllib.parse

root = pathlib.Path('/home/el01/crypto_cta_manager')
config = tomllib.loads((root / 'config/cta-manager.toml').read_text())
source_id = 'binance_exec_trade09'
values = {}
for line in pathlib.Path('/home/el01/.config/crypto-cta-manager/database.env').read_text().splitlines():
    line = line.strip().removeprefix('export ').strip()
    if line and not line.startswith('#') and '=' in line:
        name, value = line.split('=', 1)
        values[name] = value.strip().strip('"').strip("'")
url = urllib.parse.urlsplit(values[config['database']['url_env']])
db_env = os.environ.copy()
db_env.update(PGHOST=url.hostname, PGPORT=str(url.port),
              PGUSER=urllib.parse.unquote(url.username),
              PGPASSWORD=urllib.parse.unquote(url.password),
              PGDATABASE=url.path.lstrip('/'), PGCONNECT_TIMEOUT='5')


def emit(kind, **fields):
    print(json.dumps({'kind': kind, **fields}, ensure_ascii=False), flush=True)


def sql(query):
    result = subprocess.run(['/home/el01/.local/opt/postgresql/16/bin/psql',
                             '-X', '-A', '-t', '-v', 'ON_ERROR_STOP=1'],
                            input=query, env=db_env, capture_output=True, text=True)
    if result.returncode:
        raise RuntimeError('database verification failed')
    return result.stdout.strip()


def get(path, token=None):
    connection = http.client.HTTPConnection('127.0.0.1', 18201, timeout=20)
    try:
        headers = {'Cookie': 'cta_session=' + token} if token else {}
        connection.request('GET', path, headers=headers)
        response = connection.getresponse()
        return response.status, json.loads(response.read())
    finally:
        connection.close()


def funding_state():
    return json.loads(sql("SELECT json_build_object("
                          "'accounts',(SELECT count(*) FROM cta_funding_accounts),"
                          "'symbols',(SELECT count(*) FROM cta_funding_symbols WHERE source_id='binance_exec_trade09'),"
                          "'synced_symbols',(SELECT count(*) FROM cta_funding_symbols WHERE source_id='binance_exec_trade09' AND synced_through_ts_us IS NOT NULL),"
                          "'records',(SELECT count(*) FROM cta_funding_income WHERE source_id='binance_exec_trade09'),"
                          "'observation_start_us',(SELECT start_ts_us FROM cta_funding_accounts WHERE source_id='binance_exec_trade09'),"
                          "'synced_through_us',(SELECT min(synced_through_ts_us) FROM cta_funding_symbols WHERE source_id='binance_exec_trade09'));"))


def https_bindings():
    pid = subprocess.check_output(['systemctl', '--user', 'show',
                                   'crypto-cta-manager-web.service',
                                   '--property=MainPID', '--value'], text=True).strip()
    proc = pathlib.Path('/proc') / pid
    inodes = set()
    for fd in (proc / 'fd').iterdir():
        try:
            name = os.readlink(fd)
            if name.startswith('socket:['):
                inodes.add(name[8:-1])
        except OSError:
            pass
    addresses = set()
    for line in (proc / 'net/tcp').read_text().splitlines()[1:]:
        fields = line.split()
        if fields[9] in inodes and int(fields[2].split(':')[1], 16) == 443:
            addresses.add(socket.inet_ntoa(bytes.fromhex(fields[1].split(':')[0])[::-1]))
    return sorted(addresses)


deadline = time.monotonic() + 180
while True:
    try:
        status, body = get('/api/auth/status')
        if status == 200:
            break
    except (OSError, http.client.HTTPException):
        pass
    assert time.monotonic() < deadline, 'Manager did not become ready'
    emit('waiting_for_manager')
    time.sleep(15)

token = base64.urlsafe_b64encode(os.urandom(32)).decode().rstrip('=')
digest = hashlib.sha256(token.encode()).hexdigest()
session_id = sql("INSERT INTO cta_auth_sessions(user_id,token_hash,expires_at) "
                 "SELECT user_id,decode('" + digest + "','hex'),now()+interval '10 minutes' "
                 "FROM cta_users WHERE role='admin' AND disabled=false ORDER BY user_id LIMIT 1 "
                 "RETURNING session_id;").splitlines()[0]
assert session_id.isdigit()
try:
    status, health = get('/api/health', token)
    emit('health', http=status, **health)
    assert status == 200 and health['status'] == 'ok'
    deadline = time.monotonic() + 300
    saw_dedicated_socket = False
    while True:
        current = funding_state()
        addresses = https_bindings()
        saw_dedicated_socket |= '154.197.32.9' in addresses
        emit('funding_progress', **current, manager_https_local_ips=addresses)
        status, dashboard = get('/api/dashboard', token)
        account = next(a for a in dashboard['accounts'] if a['source_id'] == source_id)
        summary = account['funding']
        assert summary['last_error'] is None, 'funding collector reported an error'
        assert current['observation_start_us'] == 1791990889963576, 'observation start changed'
        if current['symbols'] > 0 and current['symbols'] == current['synced_symbols'] and summary['status'] == 'ok':
            break
        assert time.monotonic() < deadline, 'funding verification timed out'
        time.sleep(15)
    source = next(s for s in dashboard['report']['sources'] if s['source_id'] == source_id)
    enabled_sources = [a['source_id'] for a in dashboard['accounts'] if a['funding']['enabled']]
    assert enabled_sources == [source_id]
    assert saw_dedicated_socket, 'did not observe a .9-bound Manager HTTPS socket'
    emit('dashboard_funding', http=status, status=summary['status'],
         symbol_count=len(summary['symbols']), record_count=summary['record_count'],
         observation_start_us=summary['start_ts_us'], synced_through_us=summary['synced_through_ts_us'],
         last_success_us=summary['last_success_ts_us'], funding_enabled_sources=enabled_sources,
         actual_venues=sorted({v['venue'] for s in source['symbols'] for v in s['venues']}),
         dedicated_funding_socket_observed=saw_dedicated_socket)
    end_ms = int(time.time() * 1000)
    query = urllib.parse.urlencode({'sourceIds': source_id,
                                    'startMs': summary['start_ts_us'] // 1000,
                                    'endMs': end_ms, 'maxPoints': 300,
                                    'includeTheoretical': 'false'})
    status, timeline = get('/api/account-timeline?' + query, token)
    funding = timeline['funding']
    factual = timeline['report']['summary']
    income = funding['summary']['income_quote']
    before_ok = abs(funding['total_nav_before_fee_quote'] - factual['nav_change_before_fee_quote'] - income) < 1e-8
    after_ok = abs(funding['total_nav_after_fee_quote'] - factual['nav_change_after_fee_quote'] - income) < 1e-8
    assert status == 200 and before_ok and after_ok
    emit('account_timeline', http=status, funding_status=funding['summary']['status'],
         points=len(funding['points']), overall_before_additive=before_ok,
         overall_after_additive=after_ok, execution_slippage_present='execution_slippage' in timeline)
    state = json.loads((root / '.funding-backup-20261014T150804Z/state.json').read_text())
    changed = []
    checked = 0
    for process in state['processes']:
        if pathlib.Path(process['executable']).name == 'postgres':
            continue  # Manager's database client backends end when Manager restarts.
        checked += 1
        try:
            ticks = (pathlib.Path('/proc') / str(process['pid']) / 'stat').read_text().rsplit(')', 1)[1].split()[19]
        except (OSError, IndexError):
            ticks = None
        if ticks != process['start_ticks']:
            changed.append(process['pid'])
    hashes_ok = all(hashlib.sha256(pathlib.Path(p).read_bytes()).hexdigest() == h
                    for p, h in state['trade_configs'].items())
    assert not changed and hashes_ok
    emit('protected_runtime', processes_checked=checked, changed_pids=changed,
         trade_configs_checked=len(state['trade_configs']), trade_configs_unchanged=hashes_ok)
    emit('release', frontend_release=os.path.realpath(root / 'webroot'),
         binary_sha256=hashlib.sha256((root / 'bin/cta_web').read_bytes()).hexdigest(),
         config_sha256=hashlib.sha256((root / 'config/cta-manager.toml').read_bytes()).hexdigest(),
         verified_at_utc=datetime.datetime.now(datetime.timezone.utc).isoformat())
finally:
    sql('DELETE FROM cta_auth_sessions WHERE session_id=' + session_id + ';')
    assert sql('SELECT count(*) FROM cta_auth_sessions WHERE session_id=' + session_id + ';') == '0'
    emit('temporary_session_removed', removed=True)
