stage = ROOT / 'deploy-staging/trade05_allocation_20261021'
runenv = dict(os.environ)
name = cfg.get('database', {}).get('url_env', 'CRYPTO_CTA_LOCAL_DATABASE_URL')
runenv[name] = runtime[name.encode()].decode()
args = [str(stage / 'nav_allocation_reconcile'), '--config', str(ROOT / 'config/cta-manager.toml'), '--source', SOURCE, '--snapshot-json', str(stage / 'before_exec_snapshot.json'), '--manager-db', str(ROOT / 'db'), '--verification-json', str(stage / 'candidate_verification.json')]
r = subprocess.run(args, env=runenv, text=True, capture_output=True)
if r.returncode:
    print(r.stderr[-2500:])
    raise SystemExit(r.returncode)
candidate = json.loads(r.stdout)
(stage / 'candidate.json').write_text(r.stdout)
emit('candidate', {'snapshot_ts_us': candidate['snapshot_ts_us'], 'positions': len(candidate['positions']), 'symbols': len({p['symbol'] for p in candidate['positions']})})
