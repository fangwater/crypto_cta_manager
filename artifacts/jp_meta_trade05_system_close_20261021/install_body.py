import shutil
stage=ROOT/'deploy-staging/trade05_allocation_20261021'
backup=ROOT/'backups/trade05_allocation_20261021'
protected=json.loads((backup/'protected.json').read_text())
assert pid==protected['manager_pid'],'Manager changed since baseline'
assert hashlib.sha256((ROOT/'bin/cta_web').read_bytes()).hexdigest()==protected['manager_sha256']
assert hashlib.sha256((stage/'cta_web').read_bytes()).hexdigest()=='d3c7fb7324669288eac224e78bd6acaf8fc6d2d5d4fb93e44ddc999ca6480926'
for name,meta in protected['processes'].items():
    process=Path('/proc')/name
    assert process.exists(),'protected process disappeared: '+name
    assert (process/'stat').read_text().rsplit(')',1)[1].split()[19]==meta['start_ticks']
for path,digest in protected['config_hashes'].items():
    assert hashlib.sha256(Path(path).read_bytes()).hexdigest()==digest,'configuration changed'
assert not (backup/'cta_web.previous').exists()
os.link(ROOT/'bin/cta_web',backup/'cta_web.previous')
web=ROOT/'web-releases/20261021T122000ZAllocation'
web.mkdir(exist_ok=False)
subprocess.run(['tar','-xzf',str(stage/'frontend.tar.gz'),'-C',str(web)],check=True)
assert (web/'index.html').is_file()
old_web=str((ROOT/'webroot').resolve())
(backup/'rollback.json').write_text(json.dumps({'webroot':old_web,'binary':str(backup/'cta_web.previous')}))
os.replace(stage/'cta_web',ROOT/'bin/cta_web')
link=ROOT/'webroot.allocation-next';link.symlink_to(web);os.replace(link,ROOT/'webroot')
unit='crypto-cta-manager-web.service'
try:
    subprocess.run(['systemctl','--user','restart',unit],check=True)
    subprocess.run(['systemctl','--user','is-active','--quiet',unit],check=True)
except Exception:
    rollback=stage/'cta_web.rollback';shutil.copy2(backup/'cta_web.previous',rollback);os.replace(rollback,ROOT/'bin/cta_web')
    link.symlink_to(old_web);os.replace(link,ROOT/'webroot')
    subprocess.run(['systemctl','--user','restart',unit],check=True)
    raise
emit('installed',{'manager_pid':subprocess.check_output(['systemctl','--user','show',unit,'-p','MainPID','--value'],text=True).strip(),'webroot':str(web),'binary_sha256':hashlib.sha256((ROOT/'bin/cta_web').read_bytes()).hexdigest(),'allocation_rows':sql('SELECT count(*) FROM cta_strategy_allocation_snapshots;')})
