WORK = ROOT/'backups/multiple_virtuals_20261022'
RELEASE = None
CONFIG_TABLES = ['cta_account_follows', 'cta_account_strategy_bindings',
                'cta_virtual_accounts', 'cta_virtual_account_bindings', 'cta_virtual_account_managers']


def table_digests():
    return {table: json.loads(sql("SELECT json_build_object('count',count(*),'hash',md5(coalesce(string_agg(row_to_json(t)::text,E'\\n' ORDER BY row_to_json(t)::text),''))) FROM "+table+' t;')) for table in CONFIG_TABLES}


def live_baseline():
    result = protected()
    result.update(config_sha256=hashlib.sha256(CONFIG.read_bytes()).hexdigest(),
                  configurations=table_digests(), webroot_link=os.readlink(ROOT/'webroot'))
    return result


phase = sys.argv[1]
if phase == 'before':
    WORK.mkdir(mode=0o700, exist_ok=False)
    baseline = live_baseline()
    (WORK/'before.json').write_text(json.dumps(baseline, indent=2)+'\n')
    for name in ['cta_web','nav_rebuild','nav_snapshot','nav_strategy_snapshot']:
        os.link(ROOT/'bin'/name, WORK/(name+'.previous'))
    backup = WORK/'manager.pgdump'
    result = subprocess.run(['pg_dump', '--format=custom', '--file='+str(backup)], env=env, capture_output=True)
    assert result.returncode == 0, 'database backup failed; diagnostics suppressed'
    os.chmod(backup, 0o600)
    assert backup.stat().st_size > 0
    follow_rows = json.loads(sql('SELECT coalesce(json_agg(t ORDER BY source_id),\'[]\') FROM cta_account_follows t;'))
    pk = sql("SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conrelid='cta_account_follows'::regclass AND contype='p';")
    assert pk == 'PRIMARY KEY (source_id)', pk
    emit('baseline', dict(manager_pid=pid, protected_processes=len(baseline['processes']), configuration_count=len(baseline['config_hashes']),
                         follow_rows=follow_rows, primary_key=pk, backup_bytes=backup.stat().st_size))
elif phase == 'maintain':
    before=json.loads((WORK/'before.json').read_text())
    assert pid==before['manager_pid'] and hashlib.sha256(CONFIG.read_bytes()).hexdigest()==before['config_sha256'], 'live configuration changed during staging'
    assert table_digests()==before['configurations'], 'virtual configuration changed during staging'
    manifest=json.loads((WORK/'manifest.json').read_text())
    assert hashlib.sha256((WORK/'maintenance.sql').read_bytes()).hexdigest()==manifest['files_sha256']['scripts/apply_multiple_virtual_follows_schema.sql']
    assert hashlib.sha256((WORK/'allocation.sql').read_bytes()).hexdigest()==manifest['files_sha256']['scripts/apply_allocation_schema.sql']
    subprocess.run(['systemctl','--user','stop',UNIT],check=True)
    try:
        statements=[(WORK/name).read_text().replace('BEGIN;', '', 1).rsplit('COMMIT;', 1)[0] for name in ['allocation.sql','maintenance.sql']]
        sql('BEGIN; SET LOCAL lock_timeout=\'5s\'; SET LOCAL statement_timeout=\'30s\';\n'+'\n'.join(statements)+'\nCOMMIT;',readonly=False)
        assert table_digests()==before['configurations'], 'schema maintenance altered configuration rows'
    except BaseException:
        subprocess.run(['systemctl','--user','start',UNIT],check=True)
        raise
    emit('maintenance',dict(old_pid=pid,primary_key='PRIMARY KEY (source_id, virtual_id)',configuration_rows_unchanged=True))
elif phase == 'after':
    before=json.loads((WORK/'before.json').read_text())
    manifest=json.loads((WORK/'manifest.json').read_text())
    with gateway() as get:
        health=get('health')
        assert health['status']=='ok'
        emit('health',health)
        follows=json.loads(sql("SELECT coalesce(json_agg(t ORDER BY source_id,virtual_id),'[]') FROM cta_account_follows t;"))
        bindings=json.loads(sql("SELECT coalesce(json_agg(t ORDER BY source_id,binding_name),'[]') FROM cta_account_strategy_bindings t;"))
        for source in cfg['sources']:
            if not source.get('enabled',True):continue
            name=source['id']
            studio=get('catalog/accounts/'+name)
            expected=[dict(virtual_id=row['virtual_id'],multiplier=row['multiplier']) for row in follows if row['source_id']==name]
            assert studio['configuration']==(dict(mode='follow',follows=expected) if expected else dict(mode='independent'))
            assert sorted(studio['bindings'],key=lambda b:b['binding_name'])==[b for b in bindings if b['source_id']==name]
            emit('account',dict(source=name,configuration=studio['configuration'],bindings=len(studio['bindings']),pending_publishes=studio['pending_publishes']))
        virtuals=get('catalog/virtual-accounts')
        emit('virtuals',dict(count=len(virtuals),followers=sum(len(v['followers']) for v in virtuals)))
        emit('kline_status',get('catalog/kline-status'))
    after=live_baseline()
    changes={key:[p for p,value in before[key].items() if after[key].get(p)!=value] for key in ['processes','config_hashes','tables','configurations']}
    unchanged={key:after[key]==before[key] for key in ['config_sha256','funding_settings','manager_affinity']}
    RELEASE=Path(after['webroot'])
    assert after['binary_sha256']==manifest['binary_sha256'] and after['webroot']!=before['webroot']
    for name,digest in manifest['binary_files_sha256'].items():
        assert hashlib.sha256((ROOT/'bin'/name).read_bytes()).hexdigest()==digest
    opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
    for path,digest in manifest['frontend_files_sha256'].items():
        url='http://127.0.0.1:10051/manager/'+('' if path=='index.html' else path)
        with opener.open(urllib.request.Request(url,headers={'Accept-Encoding':'identity'}),timeout=30) as response:
            assert hashlib.sha256(response.read()).hexdigest()==digest
    emit('frontend',dict(verified_resources=len(manifest['frontend_files_sha256'])))
    pk=sql("SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conrelid='cta_account_follows'::regclass AND contype='p';")
    assert pk=='PRIMARY KEY (source_id, virtual_id)'
    assert sql('SELECT count(*) FROM cta_strategy_allocation_snapshots;')=='0'
    result=dict(manager_pid=pid,primary_key=pk,protected_processes=len(before['processes']),changes=changes,unchanged=unchanged)
    emit('verification',result)
    assert not any(changes.values()) and all(unchanged.values())
    (WORK/'verification.json').write_text(json.dumps(result,indent=2)+'\n')
    (RELEASE/'deployment-manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
    shutil.copy2(WORK/'maintenance.sql',RELEASE/'maintenance.sql')
    shutil.copy2(WORK/'allocation.sql',RELEASE/'allocation.sql')
    (RELEASE/'verification.json').write_text(json.dumps(result,indent=2)+'\n')
elif phase == 'cleanup':
    result=json.loads((WORK/'verification.json').read_text())
    assert result['manager_pid']==pid and not any(result['changes'].values()) and all(result['unchanged'].values())
    names=['before.json','cta_web.previous','nav_rebuild.previous','nav_snapshot.previous','nav_strategy_snapshot.previous','manager.pgdump','manifest.json','maintenance.sql','allocation.sql','verification.json','nav_window.json']
    assert {p.name for p in WORK.iterdir()}==set(names)
    for name in names:
        path=WORK/name
        assert path.is_file() and not path.is_symlink()
        path.unlink()
    WORK.rmdir()
    assert not WORK.exists() and not (ROOT/'bin/cta_web.next.multiple_virtuals_20261022').exists()
    emit('cleanup',dict(backup_directory_absent=True,release=str((ROOT/'webroot').resolve())))
