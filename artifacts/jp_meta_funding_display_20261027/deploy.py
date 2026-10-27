"""Stage checked artifacts and switch only jp-meta Manager, preserving its unit."""
import hashlib
import json
from pathlib import Path
import subprocess
from datetime import datetime, timezone

OUT = Path(__file__).resolve().parent
PROJECT = OUT.parent.parent
RELEASE = 'funding-display-'+datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ')
ROOT = '/home/ubuntu/crypto_cta_manager'
STAGE = ROOT+'/releases/'+RELEASE
binary = Path('/mnt/nvme-raid0-28t/fanghaizhou/cargo-target/crypto_cta_manager/release/cta_web')
sha = lambda p: hashlib.sha256(p.read_bytes()).hexdigest()
before = [json.loads(s) for s in (OUT/'before.jsonl').read_text().splitlines()]
assert len([r for r in before if r['name']=='query']) == 16
assert any(r['name']=='diagnostic_session_removed' for r in before)
assert not any(r['name']=='query_error' for r in before)
protected = next(r['body'] for r in before if r['name']=='protected')
manifest = dict(source_commit=subprocess.check_output(['git','rev-parse','HEAD'],cwd=PROJECT,text=True).strip(), release=RELEASE,
                binary_sha256=sha(binary), previous_binary_sha256=protected['binary_sha256'],
                frontend={str(p.relative_to(PROJECT/'frontend/dist')):sha(p) for p in (PROJECT/'frontend/dist').rglob('*') if p.is_file()})
(OUT/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
subprocess.run(['ssh','jp-meta-elvpn','install','-d','-m','0700',STAGE],check=True)
subprocess.run(['scp','-q',str(binary),str(OUT/'manifest.json'),'jp-meta-elvpn:'+STAGE+'/'],check=True)
subprocess.run(['rsync','-a',str(PROJECT/'frontend/dist')+'/','jp-meta-elvpn:'+STAGE+'/frontend/'],check=True)
remote = r'''
import hashlib,json,os,pathlib,shutil,subprocess,tomllib
root=pathlib.Path('/home/ubuntu/crypto_cta_manager')
stage=root/'releases'/RELEASE
manifest=json.loads((stage/'manifest.json').read_text())
sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
assert sha(stage/'cta_web')==manifest['binary_sha256']
assert sha(root/'bin/cta_web')==manifest['previous_binary_sha256']
for name,value in manifest['frontend'].items():
    assert sha(stage/'frontend'/name)==value
config=root/'config/cta-manager.toml'
old=config.read_text()
cfg=tomllib.loads(old)
assert cfg['funding_rates']['daily_estimates_enabled']
assert sql("SELECT count(*) FROM information_schema.columns WHERE table_name='cta_strategy_allocation_snapshots' AND column_name='restatements';") == '1'
assert cfg['funding_rates']['local_ip']=='172.31.46.92'
backup=root/'backups'/RELEASE
assert not backup.exists()
backup.mkdir(mode=0o700)
os.link(root/'bin/cta_web',backup/'cta_web.previous')
shutil.copy2(config,backup/'cta-manager.toml.previous')
previous=os.readlink(root/'webroot')
(backup/'rollback.json').write_text(json.dumps(dict(previous_webroot=previous,previous_binary_sha256=sha(root/'bin/cta_web'),previous_config_sha256=sha(config)),indent=2)+'\n')
web=root/'web-releases'/RELEASE
assert not web.exists()
(web/'manager').mkdir(parents=True)
old_assets=(root/'webroot/manager/assets').resolve()
if old_assets.is_dir():
    shutil.copytree(old_assets,web/'manager/assets')
shutil.copytree(stage/'frontend',web/'manager',dirs_exist_ok=True)
shutil.copy2(stage/'manifest.json',web/'RELEASE-MANIFEST.json')
link=root/('webroot.next.'+RELEASE)
link.symlink_to('web-releases/'+RELEASE)
os.chmod(stage/'cta_web',0o755)
os.replace(stage/'cta_web',root/'bin/cta_web')
os.replace(link,root/'webroot')
assert config.read_text()==old
unit='crypto-cta-manager-web.service'
subprocess.run(['systemctl','--user','restart',unit],check=True)
subprocess.run(['systemctl','--user','--quiet','is-active',unit],check=True)
print(json.dumps(dict(release=RELEASE,backup=str(backup),binary_sha256=sha(root/'bin/cta_web'),config_sha256=sha(config),previous_webroot=previous)),flush=True)
'''
common=(OUT/'remote_common.py').read_text()
p=subprocess.run(['ssh','jp-meta-elvpn','python3','-'],input=common+'\nRELEASE='+repr(RELEASE)+'\n'+remote,text=True,capture_output=True)
if p.returncode:
    print(p.stderr)
    raise SystemExit(p.returncode)
(OUT/'deployment.json').write_text(p.stdout)
print(p.stdout,end='')
