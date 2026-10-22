STAGE = ROOT / 'deploy-staging/rapidx_three_minute_20261022'
BACKUP = ROOT / 'backups/rapidx_three_minute_20261022'
BACKUP.mkdir(mode=0o700, exist_ok=False)
os.chmod(BACKUP, 0o700)
tables = ['cta_position_snapshots', 'cta_position_snapshot_entries', 'cta_strategy_position_snapshots', 'cta_strategy_position_snapshot_entries', 'cta_account_strategy_bindings', 'cta_strategy_allocation_snapshots']
dump = subprocess.run(['pg_dump', '-Fc', '--no-owner', '--no-acl', *[arg for table in tables for arg in ['-t', table]], '-f', str(BACKUP/'snapshots_and_bindings.dump')], env=env, capture_output=True)
assert dump.returncode == 0, 'backup failed; credentials suppressed'
os.chmod(BACKUP/'snapshots_and_bindings.dump', 0o600)
protected = {'manager_pid':pid, 'manager_sha256':hashlib.sha256((ROOT/'bin/cta_web').read_bytes()).hexdigest(), 'webroot':str((ROOT/'webroot').resolve()), 'tables':{}, 'processes':{}, 'config_hashes':{}}
for table in tables:
    protected['tables'][table] = json.loads(sql("SELECT json_build_object('count',count(*),'hash',md5(coalesce(string_agg(row_to_json(t)::text,E'\\n' ORDER BY row_to_json(t)::text),''))) FROM " + table + " t;"))
for process in Path('/proc').iterdir():
    if not process.name.isdigit(): continue
    try:
        exe = str((process/'exe').resolve())
        cmd = (process/'cmdline').read_bytes()
        if any(v in cmd for v in [b'trade_engine',b'exec-pre-trade',b'persist_manager',b'exec_config_server',b'viz_server',b'account_monitor',b'nginx',b'cta_monitor',b'spread_pbs',b'redis-server',b'cta_perp',b'cta_spread']):
            stat=(process/'stat').read_text().rsplit(')',1)[1].split()
            protected['processes'][process.name]={'exe':exe,'start_ticks':stat[19]}
    except (OSError,IndexError): pass
files=[ROOT/'config/cta-manager.toml',Path('/etc/nginx/snippets/crypto_cta_manager.conf')]
files += [Path(p) for p in cfg.get('kline',{}).get('trade_engine_configs',[])]
for file in files:
    if file.is_file():protected['config_hashes'][str(file)]=hashlib.sha256(file.read_bytes()).hexdigest()
(BACKUP/'protected.json').write_text(json.dumps(protected,indent=2)+'\n')
(BACKUP/'schema_before.sql').write_text(subprocess.check_output(['pg_dump','--schema-only','--no-owner','--no-acl'],env=env,text=True))
os.chmod(BACKUP/'schema_before.sql',0o600)
emit('backup',{'path':str(BACKUP),'protected_processes':len(protected['processes']),'configuration_count':len(protected['config_hashes'])})

os.link(ROOT/"bin/cta_web", BACKUP/"cta_web.previous")
