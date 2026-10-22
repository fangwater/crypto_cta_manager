import shutil
stage=ROOT/'deploy-staging/trade05_allocation_20261021'
backup=ROOT/'backups/trade05_allocation_20261021'
assert stage.is_dir() and not stage.is_symlink()
for name in ['candidate.json','candidate_verification.json','before_exec_snapshot.json']:
    assert hashlib.sha256((stage/name).read_bytes()).digest()==hashlib.sha256((backup/name).read_bytes()).digest()
for name in ['source_delta.tar.gz','deployment_manifest.json','online_verification_summary.json','protected_after.jsonl']:
    assert (backup/name).is_file()
    os.chmod(backup/name,0o600)
assert sql("SELECT count(*) FROM cta_strategy_allocation_snapshots WHERE source_id='"+SOURCE+"' AND snapshot_ts_us=1792585051450000;")=='1'
subprocess.run(['systemctl','--user','is-active','--quiet','crypto-cta-manager-web.service'],check=True)
shutil.rmtree(stage)
emit('cleanup',{'temporary_stage_removed':not stage.exists(),'backup_retained':str(backup),'manager_active':True,'manager_pid':pid,'allocation_rows':1})
