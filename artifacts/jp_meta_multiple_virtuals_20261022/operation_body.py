WORK = ROOT/'backups/multiple_virtuals_20261022'
RELEASE = ROOT/'web-releases/multiple_virtuals_20261022'
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
    assert not RELEASE.exists()
    baseline = live_baseline()
    (WORK/'before.json').write_text(json.dumps(baseline, indent=2)+'\n')
    os.link(ROOT/'bin/cta_web', WORK/'cta_web.previous')
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
elif phase == 'install':
    import tarfile
    before = json.loads((WORK/'before.json').read_text())
    manifest = json.loads((WORK/'manifest.json').read_text())
    assert pid == before['manager_pid'] and hashlib.sha256(CONFIG.read_bytes()).hexdigest()==before['config_sha256'], 'live configuration changed during staging'
    assert table_digests()==before['configurations'], 'virtual configuration changed during staging'
    staged = ROOT/'bin/cta_web.next.multiple_virtuals_20261022'
    assert hashlib.sha256(staged.read_bytes()).hexdigest()==manifest['binary_sha256']
    assert hashlib.sha256((WORK/'maintenance.sql').read_bytes()).hexdigest()==manifest['files_sha256']['scripts/apply_multiple_virtual_follows_schema.sql']
    RELEASE.mkdir(mode=0o755)
    frontend = RELEASE/'manager'
    frontend.mkdir(mode=0o755)
    assert hashlib.sha256((WORK/'frontend.tar.gz').read_bytes()).hexdigest()==manifest['frontend_archive_sha256']
    with tarfile.open(WORK/'frontend.tar.gz') as tar:
        tar.extractall(frontend, filter='data')
    assert (frontend/'index.html').is_file()
    assert {str(p.relative_to(frontend)):hashlib.sha256(p.read_bytes()).hexdigest() for p in frontend.rglob('*') if p.is_file()}==manifest['frontend_files_sha256']
    (RELEASE/'manifest.json').write_text(json.dumps(manifest, indent=2)+'\n')
    shutil.copy2(WORK/'implementation.patch', RELEASE/'implementation.patch')
    shutil.copy2(WORK/'maintenance.sql', RELEASE/'maintenance.sql')
    # Pause configuration writers for the one-format backend/frontend replacement.
    subprocess.run(['systemctl','--user','stop',UNIT],check=True)
    try:
        sql((WORK/'maintenance.sql').read_text(), readonly=False)
        assert table_digests()==before['configurations'], 'schema maintenance altered configuration rows'
        os.chmod(staged, 0o755)
        os.replace(staged, ROOT/'bin/cta_web')
        next_link = ROOT/'webroot.next.multiple_virtuals_20261022'
        assert not next_link.exists() and not next_link.is_symlink()
        next_link.symlink_to(RELEASE.relative_to(ROOT))
        os.replace(next_link, ROOT/'webroot')
    except BaseException:
        # Before service startup no new multi-follow requests can have been accepted.
        restore = ROOT/'bin/cta_web.restore.multiple_virtuals_20261022'
        os.link(WORK/'cta_web.previous',restore)
        os.replace(restore,ROOT/'bin/cta_web')
        sql('BEGIN; ALTER TABLE cta_account_follows DROP CONSTRAINT cta_account_follows_pkey; ALTER TABLE cta_account_follows ADD PRIMARY KEY (source_id); COMMIT;',readonly=False)
        rollback_link=ROOT/'webroot.rollback.multiple_virtuals_20261022'
        rollback_link.symlink_to(before['webroot_link'])
        os.replace(rollback_link,ROOT/'webroot')
        raise
    finally:
        subprocess.run(['systemctl','--user','start',UNIT],check=True)
    emit('installed',dict(old_pid=pid,new_pid=subprocess.check_output(['systemctl','--user','show',UNIT,'-p','MainPID','--value'],text=True).strip(),binary_sha256=manifest['binary_sha256'],release=str(RELEASE)))
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
    assert after['binary_sha256']==manifest['binary_sha256'] and Path(after['webroot'])==RELEASE
    pk=sql("SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conrelid='cta_account_follows'::regclass AND contype='p';")
    assert pk=='PRIMARY KEY (source_id, virtual_id)'
    result=dict(manager_pid=pid,primary_key=pk,protected_processes=len(before['processes']),changes=changes,unchanged=unchanged)
    emit('verification',result)
    assert not any(changes.values()) and all(unchanged.values())
    (WORK/'verification.json').write_text(json.dumps(result,indent=2)+'\n')
    (RELEASE/'verification.json').write_text(json.dumps(result,indent=2)+'\n')
elif phase == 'funding-check':
    with gateway() as get:
        params=dict(startMs=1792569300000,endMs=1792655700000,sourceIds='binance_exec_trade07',includeTheoretical='false',maxPoints=60)
        started=time.monotonic()
        while True:
            result=get('account-timeline?'+urllib.parse.urlencode(params))
            estimate=result['estimated_funding']
            if not estimate['loading'] and estimate['unavailable_reason'] is None:
                assert abs(estimate['income_quote']-0.045864087842642384)<1e-10
                emit('funding', {key:estimate[key] for key in ['loading','unavailable_reason','income_quote','settlement_count']})
                break
            emit('funding_pending',dict(loading=estimate['loading'],reason=estimate['unavailable_reason']))
            assert time.monotonic()-started<600
            time.sleep(10)
        emit('kline_status',get('catalog/kline-status'))
    logs=subprocess.check_output(['journalctl','--user','-u',UNIT,'_PID='+pid,'--no-pager','-o','cat'],text=True)
    errors=[line for line in logs.splitlines() if 'funding history refresh failed' in line]
    emit('public_funding_errors',dict(count=len(errors)))
    assert not errors
elif phase == 'cleanup':
    result=json.loads((WORK/'verification.json').read_text())
    assert result['manager_pid']==pid and not any(result['changes'].values()) and all(result['unchanged'].values())
    names=['before.json','cta_web.previous','manager.pgdump','manifest.json','maintenance.sql','frontend.tar.gz','implementation.patch','verification.json']
    assert {p.name for p in WORK.iterdir()}==set(names)
    for name in names:
        path=WORK/name
        assert path.is_file() and not path.is_symlink()
        path.unlink()
    WORK.rmdir()
    assert not WORK.exists() and not (ROOT/'bin/cta_web.next.multiple_virtuals_20261022').exists()
    emit('cleanup',dict(backup_directory_absent=True,release=str(RELEASE)))
