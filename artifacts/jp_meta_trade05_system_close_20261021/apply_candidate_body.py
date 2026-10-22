stage = ROOT / 'deploy-staging/trade05_allocation_20261021'
runenv = dict(os.environ)
name = cfg.get('database', {}).get('url_env', 'CRYPTO_CTA_LOCAL_DATABASE_URL')
runenv[name] = runtime[name.encode()].decode()
assert sql("SELECT count(*) FROM cta_strategy_allocation_snapshots WHERE source_id='"+SOURCE+"';") == '0'
note = 'Operator-requested current ownership reconciliation. Settled Exec Viz snapshot 2026-10-21 12:17:31.450 UTC; recovered same-source RapidX FIFO account quantities verified; strategy-only zero-sum transfers at completed 12:16 UTC minute close; historical fills/fees, original snapshots and account FIFO unchanged. Evidence: backups/trade05_allocation_20261021 and docs/jp_meta_trade05_system_close_20261021.md.'
r = subprocess.run([str(stage/'nav_allocation_reconcile'), '--config', str(ROOT/'config/cta-manager.toml'), '--source', SOURCE, '--candidate', str(stage/'candidate.json'), '--apply', '--note', note], env=runenv, text=True, capture_output=True)
if r.returncode:
    print(r.stderr[-2500:])
    raise SystemExit(r.returncode)
emit('applied', json.loads(r.stdout))
backup = ROOT/'backups/trade05_allocation_20261021'
for name in ['candidate.json', 'candidate_verification.json', 'before_exec_snapshot.json']:
    (backup/name).write_bytes((stage/name).read_bytes())
    os.chmod(backup/name, 0o600)
emit('allocation_record', json.loads(sql("SELECT json_build_object('source_id',source_id,'snapshot_ts_us',snapshot_ts_us,'position_count',jsonb_array_length(positions),'created_at',created_at) FROM cta_strategy_allocation_snapshots WHERE source_id='"+SOURCE+"';")))
