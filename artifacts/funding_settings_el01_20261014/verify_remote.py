"""Run over SSH on el01; print only sanitized funding deployment evidence."""
import base64
import math
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


def request(path, token=None, method='GET', body=None):
    connection = http.client.HTTPConnection('127.0.0.1', 18201, timeout=90)
    try:
        headers = {'Cookie': 'cta_session=' + token} if token else {}
        if body is not None:
            headers['Content-Type'] = 'application/json'
        connection.request(method, path, body=json.dumps(body) if body is not None else None, headers=headers)
        response = connection.getresponse()
        return response.status, json.loads(response.read())
    finally:
        connection.close()


def get(path, token=None):
    return request(path, token)


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
    time.sleep(10)

state = json.loads((root / '.funding-settings-backup-20261014/state.json').read_text())
token = base64.urlsafe_b64encode(os.urandom(32)).decode().rstrip('=')
digest = hashlib.sha256(token.encode()).hexdigest()
session_id = sql("INSERT INTO cta_auth_sessions(user_id,token_hash,expires_at) "
                 "SELECT user_id,decode('" + digest + "','hex'),now()+interval '25 minutes' "
                 "FROM cta_users WHERE role='admin' AND disabled=false ORDER BY user_id LIMIT 1 "
                 "RETURNING session_id;").splitlines()[0]
assert session_id.isdigit()
path = '/api/catalog/accounts/' + source_id + '/funding'
try:
    status, health = get('/api/health', token)
    assert status == 200 and health['status'] == 'ok'
    emit('health', http=status, **health)
    status, current = get(path, token)
    assert status == 200 and not current['enabled'] and current['unavailable_reason'] is None
    assert current['local_ip'] == '154.197.32.9' and current['public_ip'] == current['local_ip']
    emit('initial_settings', enabled=current['enabled'], refresh_secs=current['refresh_secs'], local_ip=current['local_ip'], prior_record_count=current['summary']['record_count'])
    gates = {}
    for name, session, body, expected in [
        ('anonymous', None, {'enabled': True, 'refresh_secs': 300}, 401),
        ('missing_token', token, {'enabled': True, 'refresh_secs': 300}, 403),
        ('wrong_token', token, {'enabled': True, 'refresh_secs': 300, 'enable_token': 'deliberately-invalid'}, 403),
        ('interval_too_small', token, {'enabled': True, 'refresh_secs': 59}, 400),
        ('interval_too_large', token, {'enabled': True, 'refresh_secs': 86401}, 400),
    ]:
        code, _ = request(path, session, 'PUT', body)
        assert code == expected, 'unexpected funding gating response: ' + name
        gates[name] = code
    assert sql("SELECT funding_enabled FROM cta_order_sources WHERE source_id='binance_exec_trade09';") == 'f'
    emit('enable_gates', rejected_http=gates, remains_disabled=True)
    enable_body = {'enabled': True, 'refresh_secs': 300, 'enable_token': values[config['funding']['enable_token_env']]}
    code, enabled = request(path, token, 'PUT', enable_body)
    assert code == 200 and enabled['enabled'], 'fixed-IP and REST enable test failed'
    assert enabled['verified_at_us'] is not None and enabled['verified_local_ip'] == '154.197.32.9'
    assert enabled['verified_public_ip'] == '154.197.32.9'
    verified_at = enabled['verified_at_us']
    emit('verified_enable', http=code, enabled=True, verified_at_us=verified_at, local_ip=enabled['verified_local_ip'], public_ip=enabled['verified_public_ip'])
    code, updated = request(path, token, 'PUT', {'enabled': True, 'refresh_secs': 600})
    assert code == 200 and updated['refresh_secs'] == 600 and updated['verified_at_us'] == verified_at
    code, paused = request(path, token, 'PUT', {'enabled': False, 'refresh_secs': 300})
    assert code == 200 and not paused['enabled'] and paused['summary']['record_count'] >= state['income_before']['count']
    fixed_end_ms = int(time.time()*1000)
    query = urllib.parse.urlencode({'sourceIds':source_id,'startMs':fixed_end_ms-86400000,'endMs':fixed_end_ms,'maxPoints':300,'includeTheoretical':'false'})
    code, paused_timeline = get('/api/account-timeline?' + query, token)
    f = paused_timeline['funding']; factual = paused_timeline['report']['summary']
    assert code == 200 and not f['summary']['enabled'] and f['summary']['record_count'] >= state['income_before']['count']
    assert math.isclose(f['total_nav_after_fee_quote'],factual['nav_change_after_fee_quote']+f['summary']['income_quote'],abs_tol=1e-8)
    code, _ = request(path, token, 'PUT', {'enabled':True,'refresh_secs':300})
    assert code == 403
    emit('dynamic_settings', interval_change_without_restart=True, interval_test_secs=600, paused_records_retained=f['summary']['record_count'], paused_pnl_additive=True, reenable_requires_token=True)
    code, enabled = request(path, token, 'PUT', enable_body)
    assert code == 200 and enabled['enabled'] and enabled['refresh_secs'] == 300
    assert enabled['verified_at_us'] >= verified_at
    emit('reenabled_after_tests', http=code, refresh_secs=enabled['refresh_secs'], new_verified_at_us=enabled['verified_at_us'])
    deadline = time.monotonic() + 1200
    saw_dedicated_socket = False
    while True:
        status, settings = get(path, token)
        assert status == 200
        summary = settings['summary']
        current = funding_state()
        addresses = https_bindings()
        saw_dedicated_socket |= '154.197.32.9' in addresses
        emit('funding_progress', status=summary['status'], **current, manager_https_local_ips=addresses)
        assert summary['last_error'] is None, 'funding collector reported an error'
        assert settings['enabled'] and settings['refresh_secs'] == 300
        if summary['status'] == 'ok':
            break
        assert time.monotonic() < deadline, 'historical funding did not finish'
        time.sleep(15)
    earliest_index = int(sql("SELECT min(first_event_ts_us) FROM cta_source_symbols WHERE source_id='binance_exec_trade09';"))
    assert summary['start_ts_us'] <= earliest_index and summary['start_ts_us'] >= earliest_index-60000000
    assert summary['start_ts_us'] < 1791990889963576
    assert len(summary['symbols']) == 15 and all(s['synced_through_ts_us'] is not None for s in summary['symbols'])
    assert current['records'] > state['income_before']['count']
    preserved = json.loads(sql("SELECT coalesce(json_agg(json_build_object('venue',venue,'tran_id',tran_id,'income',income::text) ORDER BY venue,tran_id),'[]') FROM cta_funding_income WHERE source_id='binance_exec_trade09';"))
    assert all(row in preserved for row in state['preserved_income'])
    status, dashboard = get('/api/dashboard', token)
    enabled_sources = [a['source_id'] for a in dashboard['accounts'] if a['funding']['enabled']]
    assert status == 200 and enabled_sources == [source_id]
    assert saw_dedicated_socket, 'did not observe a .9-bound Manager socket'
    emit('history_completed', status=summary['status'], start_ts_us=summary['start_ts_us'], start_utc=datetime.datetime.fromtimestamp(summary['start_ts_us']/1000000,datetime.timezone.utc).isoformat(), synced_through_us=summary['synced_through_ts_us'], symbols=len(summary['symbols']), records=summary['record_count'], income_quote=summary['income_quote'], original_records_preserved=True, only_enabled_source=enabled_sources, dedicated_socket_observed=saw_dedicated_socket)
    # Query the available account NAV interval; funding uses the identical bounds.
    end_ms=int(time.time()*1000)
    query=urllib.parse.urlencode({'sourceIds':source_id,'endMs':end_ms,'maxPoints':300,'includeTheoretical':'false'})
    status, timeline = get('/api/account-timeline?' + query, token)
    funding=timeline['funding']; factual=timeline['report']['summary']; income=funding['summary']['income_quote']
    assert status==200
    expected=json.loads(sql("SELECT json_build_object('count',count(*),'income',coalesce(sum(income),0)::double precision) FROM cta_funding_income WHERE source_id='binance_exec_trade09' AND ts_us BETWEEN "+str(timeline['report']['start_ts_us'])+" AND "+str(timeline['report']['end_ts_us'])+";"))
    assert funding['summary']['record_count']==expected['count'] and math.isclose(income,expected['income'],abs_tol=1e-8)
    before_ok=math.isclose(funding['total_nav_before_fee_quote'],factual['nav_change_before_fee_quote']+income,abs_tol=1e-8)
    after_ok=math.isclose(funding['total_nav_after_fee_quote'],factual['nav_change_after_fee_quote']+income,abs_tol=1e-8)
    assert before_ok and after_ok and len(funding['points'])>0
    emit('account_timeline', http=status, record_count=funding['summary']['record_count'], funding_income_quote=income, points=len(funding['points']), overall_before_additive=before_ok,overall_after_additive=after_ok)
    changed=[];checked=0
    postgres_processes=[p for p in state['processes'] if pathlib.Path(p['executable']).name=='postgres']
    master=min(postgres_processes,key=lambda p:int(p['start_ticks']))
    protected=[p for p in state['processes'] if pathlib.Path(p['executable']).name!='postgres']+[master]
    for process in protected:
        checked+=1
        try:
            ticks=(pathlib.Path('/proc')/str(process['pid'])/'stat').read_text().rsplit(')',1)[1].split()[19]
        except (OSError,IndexError):ticks=None
        if ticks!=process['start_ticks']:changed.append(process['pid'])
    hashes_ok=all(hashlib.sha256(pathlib.Path(p).read_bytes()).hexdigest()==h for p,h in state['trade_configs'].items())
    assert not changed and hashes_ok
    emit('protected_runtime',processes_checked=checked,changed_pids=changed,trade_configs_checked=len(state['trade_configs']),trade_configs_unchanged=hashes_ok)
    connection=http.client.HTTPConnection('127.0.0.1',10051,timeout=10)
    connection.request('GET','/manager/'); response=connection.getresponse(); page=response.read().decode(); connection.close()
    assert response.status==200
    import re
    assets=re.findall(r'(?:src|href)="(/manager/assets/[^"]+)"',page)
    for asset in assets:
        connection=http.client.HTTPConnection('127.0.0.1',10051,timeout=10);connection.request('GET',asset);response=connection.getresponse();content=response.read();connection.close();assert response.status==200
        if asset.endswith('.js'):assert '测试出口并开启'.encode() in content and 'Funding 记录'.encode() in content
    emit('frontend_served',http=200,assets=len(assets),configuration_controls_present=True)
    for name,port,path in [('viz',10049,'/snapshot'),('config',18169,'/')]:
        connection=http.client.HTTPConnection('127.0.0.1',port,timeout=10);connection.request('GET',path);response=connection.getresponse();response.read();connection.close();assert response.status==200
        emit('exec_endpoint',name=name,http=response.status)
    emit('release',frontend_release=os.path.realpath(root/'webroot'),binary_sha256=hashlib.sha256((root/'bin/cta_web').read_bytes()).hexdigest(),config_sha256=hashlib.sha256((root/'config/cta-manager.toml').read_bytes()).hexdigest(),verified_at_utc=datetime.datetime.now(datetime.timezone.utc).isoformat())
finally:
    sql('DELETE FROM cta_auth_sessions WHERE session_id='+session_id+';')
    assert sql('SELECT count(*) FROM cta_auth_sessions WHERE session_id='+session_id+';')=='0'
    emit('temporary_session_removed',removed=True)
