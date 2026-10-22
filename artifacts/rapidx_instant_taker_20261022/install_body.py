import shutil
stage=ROOT/'deploy-staging/rapidx_instant_taker_20261022'
backup=ROOT/'backups/rapidx_instant_taker_20261022'
manifest=json.loads((stage/'release.json').read_text())
protected=json.loads((backup/'protected.json').read_text())
assert pid==protected['manager_pid'],'Manager changed since backup'
assert hashlib.sha256((ROOT/'bin/cta_web').read_bytes()).hexdigest()==protected['manager_sha256']
assert hashlib.sha256((stage/'cta_web').read_bytes()).hexdigest()==manifest['binary_sha256']
for name,meta in protected['processes'].items():
    process=Path('/proc')/name
    assert process.exists(),'protected process disappeared: '+name
    assert (process/'stat').read_text().rsplit(')',1)[1].split()[19]==meta['start_ticks']
for name,digest in protected['config_hashes'].items():
    assert hashlib.sha256(Path(name).read_bytes()).hexdigest()==digest,'configuration changed'
web=ROOT/'web-releases'/manifest['release']
web.mkdir(exist_ok=False)
subprocess.run(['tar','-xzf',str(stage/'frontend.tar.gz'),'-C',str(web)],check=True)
assert (web/'manager/index.html').is_file()
for name,digest in manifest['frontend'].items():
    assert hashlib.sha256((web/'manager'/name).read_bytes()).hexdigest()==digest
(web/'RELEASE-MANIFEST.json').write_text(json.dumps(manifest,indent=2)+'\n')
old_web=protected['webroot']
(backup/'rollback.json').write_text(json.dumps({'webroot':old_web,'binary':str(backup/'cta_web.previous')}))
os.chmod(stage/'cta_web',0o755)
os.replace(stage/'cta_web',ROOT/'bin/cta_web')
link=ROOT/'webroot.instant-taker-next';link.symlink_to(web);os.replace(link,ROOT/'webroot')
unit='crypto-cta-manager-web.service'
try:
    subprocess.run(['systemctl','--user','restart',unit],check=True)
    subprocess.run(['systemctl','--user','is-active','--quiet',unit],check=True)
except Exception:
    rollback=stage/'cta_web.rollback';shutil.copy2(backup/'cta_web.previous',rollback);os.replace(rollback,ROOT/'bin/cta_web')
    link.symlink_to(old_web);os.replace(link,ROOT/'webroot')
    subprocess.run(['systemctl','--user','restart',unit],check=True)
    raise
emit('installed',manifest)
