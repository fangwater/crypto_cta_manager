backup=ROOT/'backups/trade05_allocation_20261021'
protected=json.loads((backup/'protected.json').read_text())
results={'processes':{},'configurations':{},'tables':{}}
for name,old in protected['processes'].items():
    process=Path('/proc')/name
    try:
        new={'exe':str((process/'exe').resolve()),'start_ticks':(process/'stat').read_text().rsplit(')',1)[1].split()[19]}
        results['processes'][name]=new==old
    except OSError: results['processes'][name]=False
for path,digest in protected['config_hashes'].items():
    results['configurations'][path]=hashlib.sha256(Path(path).read_bytes()).hexdigest()==digest
for table,old in protected['tables'].items():
    new=json.loads(sql("SELECT json_build_object('count',count(*),'hash',md5(coalesce(string_agg(row_to_json(t)::text,E'\\n' ORDER BY row_to_json(t)::text),''))) FROM "+table+" t;"))
    results['tables'][table]={'unchanged':new==old,'before':old,'after':new}
results['manager']={'pid':pid,'binary_sha256':hashlib.sha256((ROOT/'bin/cta_web').read_bytes()).hexdigest(),'webroot':str((ROOT/'webroot').resolve()),'cpu_affinity':next(l.strip() for l in Path('/proc/'+pid+'/status').read_text().splitlines() if l.startswith('Cpus_allowed_list:'))}
results['allocations']=json.loads(sql("SELECT coalesce(json_agg(r),'[]'::json) FROM (SELECT source_id,snapshot_ts_us,jsonb_array_length(positions) AS position_count FROM cta_strategy_allocation_snapshots) r;"))
emit('protected',results)
assert all(results['processes'].values()),'protected process identity changed'
assert all(results['configurations'].values()),'configuration changed'
assert all(t['unchanged'] for t in results['tables'].values()),'original snapshot/binding table changed'
