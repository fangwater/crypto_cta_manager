import contextlib
import shutil
import sys
import urllib.error

UNIT = 'crypto-cta-manager-web.service'
WORK = ROOT / 'backups/public_funding_20261022'
CONFIG = ROOT / 'config/cta-manager.toml'
LOCAL_IP = '172.31.46.92'
SOURCES = ['binance_exec_trade%02d' % n for n in range(1, 8)]
TABLES = ['cta_position_snapshots', 'cta_position_snapshot_entries',
          'cta_strategy_position_snapshots', 'cta_strategy_position_snapshot_entries',
          'cta_strategy_allocation_snapshots']


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def protected():
    state = {'manager_pid': pid, 'binary_sha256': hashlib.sha256((ROOT/'bin/cta_web').read_bytes()).hexdigest(),
             'webroot': str((ROOT/'webroot').resolve()), 'processes': {}, 'config_hashes': {}, 'tables': {}}
    for process in Path('/proc').iterdir():
        if not process.name.isdigit():
            continue
        try:
            command = (process/'cmdline').read_bytes()
            if any(name in command for name in [b'trade_engine', b'exec-pre-trade', b'persist_manager',
                    b'exec_config_server', b'viz_server', b'account_monitor', b'nginx', b'cta_monitor',
                    b'spread_pbs', b'redis-server', b'cta_perp', b'cta_spread']):
                state['processes'][process.name] = {
                    'exe': str((process/'exe').resolve()),
                    'start_ticks': (process/'stat').read_text().rsplit(')', 1)[1].split()[19],
                    'affinity': next(line for line in (process/'status').read_text().splitlines()
                                     if line.startswith('Cpus_allowed_list:'))}
        except (OSError, IndexError):
            continue
    files = [Path('/etc/nginx/snippets/crypto_cta_manager.conf')]
    files.extend(Path(p) for p in cfg.get('kline', {}).get('trade_engine_configs', []))
    for path in files:
        if path.is_file():
            state['config_hashes'][str(path)] = hashlib.sha256(path.read_bytes()).hexdigest()
    for table in TABLES:
        state['tables'][table] = json.loads(sql("SELECT json_build_object('count',count(*),'hash',md5(coalesce(string_agg(row_to_json(t)::text,E'\\n' ORDER BY row_to_json(t)::text),''))) FROM " + table + ' t;'))
    state['funding_settings'] = json.loads(sql("SELECT json_agg(t ORDER BY source_id) FROM (SELECT source_id,funding_enabled,funding_refresh_secs FROM cta_order_sources) t;"))
    state['manager_affinity'] = next(line for line in Path('/proc/'+pid+'/status').read_text().splitlines() if line.startswith('Cpus_allowed_list:'))
    return state


@contextlib.contextmanager
def gateway():
    token = secrets.token_urlsafe(32)
    token_hash = hashlib.sha256(token.encode()).hexdigest()
    session = sql("INSERT INTO cta_auth_sessions (user_id,token_hash,expires_at) SELECT user_id,decode('" + token_hash + "','hex'),now()+interval '30 minutes' FROM cta_users WHERE role='admin' AND disabled=false ORDER BY user_id LIMIT 1 RETURNING session_id;", readonly=False)
    assert session.isdigit()
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    def get(path):
        request = urllib.request.Request('http://13.115.227.29:4191/manager/api/' + path,
                                         headers={'Cookie': 'cta_session=' + token})
        began = time.monotonic()
        while True:
            try:
                with opener.open(request, timeout=55) as response:
                    return json.load(response)
            except urllib.error.HTTPError as error:
                if path != 'health' or error.code not in [502, 503, 504] or time.monotonic()-began > 600:
                    raise
                emit('startup_pending', {'http': error.code})
                time.sleep(10)

    try:
        yield get
    finally:
        sql('DELETE FROM cta_auth_sessions WHERE session_id=' + session + ';', readonly=False)
        assert sql('SELECT count(*) FROM cta_auth_sessions WHERE session_id=' + session + ';') == '0'
        emit('diagnostic_session_removed', True)


def capture(phase):
    window = json.loads((WORK/'window.json').read_text())
    result = {'window': window, 'queries': {}, 'funding': {}}
    with gateway() as get:
        result['health'] = get('health')
        emit('health', result['health'])
        for source in (SOURCES if phase == 'before' else list(reversed(SOURCES))):
            for endpoint in ['account-timeline', 'timeline']:
                params = dict(window, sourceIds=source, maxPoints=60, includeTheoretical='false')
                start = time.monotonic()
                while True:
                    try:
                        body = get(endpoint + '?' + urllib.parse.urlencode(params))
                    except urllib.error.HTTPError as error:
                        if error.code in [502, 503, 504] and time.monotonic()-start < 600:
                            emit('pending', {'source': source, 'endpoint': endpoint, 'http': error.code})
                            time.sleep(10)
                            continue
                        raise
                    estimate = body['estimated_funding']
                    if phase == 'before' or (not estimate['loading'] and not estimate['unavailable_reason']):
                        break
                    assert time.monotonic()-start < 3600, 'public funding still unavailable'
                    emit('pending', {'source': source, 'endpoint': endpoint, 'loading': estimate['loading'], 'reason': estimate['unavailable_reason']})
                    time.sleep(10)
                row = {'report_sha256': digest(body['report']), 'factual_funding_sha256': digest(body['funding']['records']),
                       'estimate': {key: estimate[key] for key in ['loading', 'unavailable_reason', 'income_quote', 'settlement_count', 'synced_through_ts_us']}}
                if phase == 'after':
                    assert estimate['income_quote'] is not None
                    if estimate['points']:
                        assert abs(estimate['points'][-1]['funding_income_quote'] - estimate['income_quote']) < 1e-8
                    assert abs(sum(s['income_quote'] for s in estimate['strategies']) - estimate['income_quote']) < 1e-7
                result['queries'][source+'_'+endpoint] = row
                emit('query', dict(source=source, endpoint=endpoint, **row))
            settings = get('catalog/accounts/' + source + '/funding')
            funding_started = time.monotonic()
            while phase == 'after' and settings['enabled'] and settings['summary']['status'] != 'ok':
                emit('factual_funding_pending', dict(source=source, status=settings['summary']['status'], last_error=settings['summary']['last_error']))
                assert time.monotonic()-funding_started < 300 and settings['summary']['last_error'] is None
                time.sleep(10)
                settings = get('catalog/accounts/' + source + '/funding')
            result['funding'][source] = {key: settings[key] for key in ['enabled', 'refresh_secs', 'local_ip', 'public_ip']}
            result['funding'][source]['summary'] = {key: settings['summary'][key] for key in ['status', 'last_error', 'record_count', 'last_success_ts_us']}
            if phase == 'after' and settings['enabled']:
                assert settings['summary']['status'] == 'ok' and settings['summary']['last_error'] is None
            emit('factual_funding', dict(source=source, **result['funding'][source]))
        result['kline_status'] = get('catalog/kline-status')
        emit('kline_status', result['kline_status'])
    (WORK/(phase+'.json')).write_text(json.dumps(result, indent=2)+'\n')
    if phase == 'after':
        before = json.loads((WORK/'before.json').read_text())
        comparison = {key: {'order_report_unchanged': value['report_sha256']==before['queries'][key]['report_sha256'],
                           'factual_funding_unchanged': value['factual_funding_sha256']==before['queries'][key]['factual_funding_sha256']}
                      for key, value in result['queries'].items()}
        emit('fixed_window_comparison', comparison)
        assert all(all(checks.values()) for checks in comparison.values())


phase = sys.argv[1]
if phase == 'before':
    WORK.mkdir(mode=0o700, exist_ok=False)
    os.chmod(WORK, 0o700)
    shutil.copy2(CONFIG, WORK/'cta-manager.toml.previous')
    os.chmod(WORK/'cta-manager.toml.previous', 0o600)
    state = protected()
    (WORK/'protected.json').write_text(json.dumps(state, indent=2)+'\n')
    end = (int(time.time())//60-30)*60000
    (WORK/'window.json').write_text(json.dumps({'startMs': end-86400000, 'endMs': end}))
    emit('backup', {'path': str(WORK), 'protected_processes': len(state['processes']), 'configurations': len(state['config_hashes']), 'manager_pid': pid})
    capture('before')
elif phase == 'apply':
    initial_ip = '172.31.46.93'
    original = (WORK/'cta-manager.toml.previous').read_bytes()
    assert CONFIG.read_bytes() == original, 'Manager config changed since backup'
    assert 'funding_rates' not in cfg
    assert cfg['kline']['local_ip'] == initial_ip and cfg['kline']['public_ip'] == '18.181.48.65'
    content = ('# Public funding history uses the existing market-data egress (2026-10-22).\n'
               '[funding_rates]\nlocal_ip = "'+initial_ip+'"\n\n').encode()+original
    parsed = tomllib.loads(content.decode())
    assert parsed.pop('funding_rates') == {'local_ip': initial_ip} and parsed == cfg
    stage = CONFIG.with_name('cta-manager.toml.next.public_funding_20261022')
    assert not stage.exists()
    stage.write_bytes(content)
    shutil.copystat(CONFIG, stage)
    os.replace(stage, CONFIG)
    stamp = time.strftime('%Y-%m-%d %H:%M:%S UTC', time.gmtime())
    (WORK/'restart_time.txt').write_text(stamp)
    subprocess.run(['systemctl', '--user', 'restart', UNIT], check=True)
    emit('applied', {'old_pid': pid, 'new_pid': subprocess.check_output(['systemctl', '--user', 'show', UNIT, '-p', 'MainPID', '--value'], text=True).strip(),
                     'restart_time': stamp, 'funding_rates': {'local_ip': initial_ip}, 'config_sha256': hashlib.sha256(content).hexdigest()})
elif phase == 'after':
    capture('after')
elif phase == 'separate-egress':
    original = CONFIG.read_bytes()
    assert cfg['funding_rates'] == {'local_ip': '172.31.46.93'}
    backup = WORK/'cta-manager.toml.shared-egress'
    assert not backup.exists()
    shutil.copy2(CONFIG, backup)
    os.chmod(backup, 0o600)
    header = ('# Public funding history uses the existing market-data egress (2026-10-22).\n'
              '[funding_rates]\nlocal_ip = "172.31.46.93"\n\n').encode()
    assert original.startswith(header)
    replacement = ('# Public funding egress: 172.31.46.92 -> 52.192.54.88 (2026-10-22).\n'
                   '# Separate from Kline; trading and factual funding bindings are unchanged.\n'
                   '[funding_rates]\nlocal_ip = "172.31.46.92"\n\n').encode()+original[len(header):]
    parsed = tomllib.loads(replacement.decode())
    expected = dict(cfg, funding_rates={'local_ip': LOCAL_IP})
    assert parsed == expected
    stage = CONFIG.with_name('cta-manager.toml.next.public_funding_20261022')
    assert not stage.exists()
    stage.write_bytes(replacement)
    shutil.copystat(CONFIG, stage)
    os.replace(stage, CONFIG)
    stamp = time.strftime('%Y-%m-%d %H:%M:%S UTC', time.gmtime())
    (WORK/'restart_time.txt').write_text(stamp)
    subprocess.run(['systemctl', '--user', 'restart', UNIT], check=True)
    emit('separate_egress', {'old_pid': pid, 'new_pid': subprocess.check_output(['systemctl', '--user', 'show', UNIT, '-p', 'MainPID', '--value'], text=True).strip(), 'restart_time': stamp, 'public_funding_local_ip': LOCAL_IP, 'public_ip': '52.192.54.88'})
elif phase == 'status':
    with gateway() as get:
        emit('health', get('health'))
        emit('kline_status', get('catalog/kline-status'))
elif phase == 'funding-health':
    with gateway() as get:
        for source in SOURCES:
            settings = get('catalog/accounts/' + source + '/funding')
            emit('funding', dict(source=source, enabled=settings['enabled'], summary={k: settings['summary'][k] for k in ['status', 'last_error', 'record_count']}))
    logs = subprocess.check_output(['journalctl', '--user', '-u', UNIT, '_PID='+pid, '--no-pager', '-o', 'cat'], text=True)
    errors = [line for line in logs.splitlines() if 'funding history refresh failed' in line or 'funding collection failed' in line]
    emit('funding_errors', dict(count=len(errors), last=errors[-3:]))
elif phase == 'sample':
    with gateway() as get:
        params = dict(json.loads((WORK/'window.json').read_text()), sourceIds='binance_exec_trade03', symbols='BTCUSDT', includeTheoretical='false', maxPoints=60)
        try:
            result = get('account-timeline?' + urllib.parse.urlencode(params))
        except urllib.error.HTTPError as error:
            emit('sample_error', {'status': error.code, 'body': error.read().decode()[:1000]})
            raise
        estimate = result['estimated_funding']
        emit('sample_funding', {key: estimate[key] for key in ['loading', 'unavailable_reason', 'income_quote', 'settlement_count', 'synced_through_ts_us']})
elif phase == 'install-binary':
    manifest = json.loads((WORK/'release.json').read_text())
    staged = ROOT/'bin/cta_web.next.public_funding_20261022'
    expected = manifest['binary_sha256']
    assert hashlib.sha256(staged.read_bytes()).hexdigest() == expected
    old = json.loads((WORK/'protected.json').read_text())
    assert hashlib.sha256((ROOT/'bin/cta_web').read_bytes()).hexdigest() == old['binary_sha256']
    assert cfg['funding_rates'] == {'local_ip': '172.31.46.93'}
    backup = WORK/'cta_web.previous'
    assert not backup.exists()
    os.link(ROOT/'bin/cta_web', backup)
    os.chmod(staged, 0o755)
    os.replace(staged, ROOT/'bin/cta_web')
    stamp = time.strftime('%Y-%m-%d %H:%M:%S UTC', time.gmtime())
    (WORK/'restart_time.txt').write_text(stamp)
    subprocess.run(['systemctl', '--user', 'restart', UNIT], check=True)
    emit('installed', {'binary_sha256': expected, 'old_pid': pid, 'new_pid': subprocess.check_output(['systemctl', '--user', 'show', UNIT, '-p', 'MainPID', '--value'], text=True).strip(), 'restart_time': stamp})
elif phase == 'tighten-budget':
    manifest = json.loads((WORK/'release.json').read_text())
    staged = ROOT/'bin/cta_web.next.public_funding_20261022'
    expected = manifest['binary_sha256']
    assert hashlib.sha256(staged.read_bytes()).hexdigest() == expected
    assert hashlib.sha256((ROOT/'bin/cta_web').read_bytes()).hexdigest() == manifest['previous_binary_sha256']
    assert cfg['funding_rates'] == {'local_ip': LOCAL_IP}
    backup = WORK/'cta_web.previous-throttle'
    assert not backup.exists()
    os.link(ROOT/'bin/cta_web', backup)
    os.chmod(staged, 0o755)
    os.replace(staged, ROOT/'bin/cta_web')
    stamp = time.strftime('%Y-%m-%d %H:%M:%S UTC', time.gmtime())
    (WORK/'restart_time.txt').write_text(stamp)
    subprocess.run(['systemctl', '--user', 'restart', UNIT], check=True)
    emit('tightened_budget', {'binary_sha256': expected, 'old_pid': pid, 'new_pid': subprocess.check_output(['systemctl', '--user', 'show', UNIT, '-p', 'MainPID', '--value'], text=True).strip(), 'restart_time': stamp, 'interval_seconds': 3, 'five_minute_budget': 90})
elif phase == 'concurrent-maintenance':
    old = json.loads((WORK/'protected.json').read_text())
    new = protected()
    previous = tomllib.loads((WORK/'cta-manager.toml.previous').read_text())
    emit('maintenance_changes', dict(manager_pid=pid, manager_started=subprocess.check_output(['systemctl','--user','show',UNIT,'-p','ExecMainStartTimestamp','--value'],text=True).strip(),
        changed_config_sections=[k for k in set(previous)|set(cfg) if previous.get(k)!=cfg.get(k)],
        sources_before=[s['id'] for s in previous['sources']], sources_now=[s['id'] for s in cfg['sources']],
        funding_settings_before=old['funding_settings'], funding_settings_now=new['funding_settings']))
elif phase == 'protected':
    old = json.loads((WORK/'protected.json').read_text())
    new = protected()
    changes = {'processes': [p for p, value in old['processes'].items() if new['processes'].get(p) != value],
               'configurations': [p for p, value in old['config_hashes'].items() if new['config_hashes'].get(p) != value],
               'tables': [p for p, value in old['tables'].items() if new['tables'].get(p) != value]}
    emit('process_differences', {p: {'before': old['processes'][p], 'after': new['processes'].get(p)} for p in changes['processes']})
    unchanged = {key: old[key] == new[key] for key in ['webroot', 'funding_settings', 'manager_affinity']}
    expected_binary = json.loads((WORK/'release.json').read_text())['binary_sha256'] if (WORK/'release.json').exists() else old['binary_sha256']
    unchanged['binary_matches_release'] = new['binary_sha256'] == expected_binary
    previous = tomllib.loads((WORK/'cta-manager.toml.previous').read_text())
    current = dict(cfg)
    public_config = current.pop('funding_rates')
    unchanged['other_manager_config'] = current == previous
    logs = subprocess.check_output(['journalctl', '--user', '-u', UNIT, '_PID='+pid, '--no-pager', '-o', 'cat'], text=True)
    funding_errors = [line for line in logs.splitlines() if 'funding history refresh failed' in line or 'funding collection failed' in line]
    recent_logs = subprocess.check_output(['journalctl', '--user', '-u', UNIT, '_PID='+pid, '--since', '5 minutes ago', '--no-pager', '-o', 'cat'], text=True)
    recent_errors = [line for line in recent_logs.splitlines() if 'funding history refresh failed' in line or 'funding collection failed' in line]
    emit('protected', {'checked_processes': len(old['processes']), 'checked_configurations': len(old['config_hashes']), 'changes': changes,
                       'unchanged': unchanged, 'new_manager_pid': pid, 'public_config': public_config, 'funding_errors_since_restart': len(funding_errors),
                       'funding_errors_recent_5m': len(recent_errors), 'funding_errors_sample': funding_errors[-3:]})
    # Operator confirmed these RX02 processes were deliberately stopped separately.
    expected_stopped = {'94898','95314','95549','95871','100190','100253','100312','163597','384442','3639659'}
    assert set(changes['processes']) == expected_stopped
    assert all(new['processes'].get(p) is None for p in expected_stopped)
    assert not changes['configurations'] and not changes['tables'] and all(unchanged.values()) and not recent_errors
    (WORK/'verification.json').write_text(json.dumps({'checked_processes': len(old['processes']), 'checked_configurations': len(old['config_hashes']), 'changes': changes, 'unchanged': unchanged, 'manager_pid': pid, 'operator_confirmed_stopped_processes': sorted(expected_stopped), 'funding_errors_since_restart': len(funding_errors), 'funding_errors_recent_5m': len(recent_errors)}, indent=2)+'\n')
elif phase == 'cleanup':
    verification = json.loads((WORK/'verification.json').read_text())
    after = json.loads((WORK/'after.json').read_text())
    before = json.loads((WORK/'before.json').read_text())
    assert len(after['queries']) == 14 and verification['manager_pid'] == pid
    for key, row in after['queries'].items():
        assert row['report_sha256'] == before['queries'][key]['report_sha256']
        assert row['factual_funding_sha256'] == before['queries'][key]['factual_funding_sha256']
        assert row['estimate']['income_quote'] is not None and not row['estimate']['unavailable_reason']
    release = ROOT/'releases/public_funding_20261022'
    release.mkdir(mode=0o755, exist_ok=False)
    manifest = json.loads((WORK/'release.json').read_text())
    manifest.update(verification=verification, window=after['window'], final_restart_time=(WORK/'restart_time.txt').read_text())
    (release/'manifest.json').write_text(json.dumps(manifest, indent=2)+'\n')
    names = ['cta-manager.toml.previous', 'cta-manager.toml.shared-egress', 'cta_web.previous',
             'cta_web.previous-throttle', 'protected.json', 'before.json', 'after.json',
             'window.json', 'release.json', 'restart_time.txt', 'verification.json']
    assert {p.name for p in WORK.iterdir()} == set(names)
    removed = {}
    for name in names:
        path = WORK/name
        assert path.is_file() and not path.is_symlink()
        removed[name] = hashlib.sha256(path.read_bytes()).hexdigest()
        path.unlink()
        assert not path.exists()
    WORK.rmdir()
    assert not WORK.exists()
    assert not (ROOT/'bin/cta_web.next.public_funding_20261022').exists()
    assert not CONFIG.with_name('cta-manager.toml.next.public_funding_20261022').exists()
    emit('cleanup', {'removed_sha256': removed, 'backup_directory_absent': True, 'release_manifest': str(release/'manifest.json')})
else:
    raise ValueError('unknown phase')
