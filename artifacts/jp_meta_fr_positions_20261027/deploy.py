"""Deploy the paired FR overview change; rollback paths are kept until verification."""
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import subprocess

OUT = Path(__file__).resolve().parent
CTA = OUT.parent.parent
NAV = Path('/home/fanghaizhou/crypto_nav_manager')
RELEASE = 'fr-positions-' + datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ')
ROOT = '/home/ubuntu/crypto_cta_manager'
STAGE = ROOT + '/releases/' + RELEASE
BINS = ['crypto_nav_manager', 'reconcile_rocksdb', 'sync_history', 'sync_fee_rates',
        'sync_intra_orders', 'sync_intra_hourly_latency', 'fee_rate_probe']
sha = lambda p: hashlib.sha256(p.read_bytes()).hexdigest()
before = [json.loads(line) for line in (OUT / 'before.jsonl').read_text().splitlines()]
previous = next(row['body'] for row in before if row['name'] == 'protected')
migrations = {p.name: sha(p) for p in (NAV / 'migrations').glob('*.sql')}
assert migrations == next(row['body'] for row in before if row['name'] == 'nav_migrations_hashes')
binary = Path('/mnt/nvme-raid0-28t/fanghaizhou/cargo-target/crypto_cta_manager/release/cta_web')
nav_bins = Path('/mnt/nvme-raid0-28t/fanghaizhou/cargo-target/crypto_nav_manager/release')
manifest = {
    'release': RELEASE,
    'cta_commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=CTA, text=True).strip(),
    'nav_commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=NAV, text=True).strip(),
    'nav_previous_commit': subprocess.check_output(['git', 'rev-parse', 'HEAD^'], cwd=NAV, text=True).strip(),
    'cta_sha256': sha(binary),
    'cta_previous_sha256': previous['binary_sha256'],
    'nav_binaries': {name: sha(nav_bins / name) for name in BINS},
    'nav_migrations': migrations,
    'frontend': {str(p.relative_to(CTA / 'frontend/dist')): sha(p)
                 for p in (CTA / 'frontend/dist').rglob('*') if p.is_file()},
}
(OUT / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
subprocess.run(['ssh', 'jp-meta-elvpn', 'install', '-d', '-m', '0700', STAGE + '/nav'], check=True)
subprocess.run(['scp', '-q', str(binary), str(OUT / 'manifest.json'), 'jp-meta-elvpn:' + STAGE + '/'], check=True)
subprocess.run(['scp', '-q', *[str(nav_bins / name) for name in BINS], 'jp-meta-elvpn:' + STAGE + '/nav/'], check=True)
subprocess.run(['rsync', '-a', str(CTA / 'frontend/dist') + '/', 'jp-meta-elvpn:' + STAGE + '/frontend/'], check=True)

remote = r'''
import hashlib,json,os,pathlib,shutil,subprocess
root=pathlib.Path('/home/ubuntu/crypto_cta_manager')
nav=pathlib.Path('/home/ubuntu/crypto_nav_manager')
stage=root/'releases'/RELEASE
m=json.loads((stage/'manifest.json').read_text())
sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
assert subprocess.check_output(['whoami'],text=True).strip()=='ubuntu'
assert subprocess.check_output(['hostname','-f'],text=True).strip()=='ip-172-31-35-228.ap-northeast-1.compute.internal'
assert not subprocess.check_output(['git','status','--porcelain'],cwd=nav,text=True).strip()
assert subprocess.check_output(['git','rev-parse','HEAD'],cwd=nav,text=True).strip()==m['nav_previous_commit']
assert sha(stage/'cta_web')==m['cta_sha256']
assert sha(root/'bin/cta_web')==m['cta_previous_sha256']
assert {p.name:sha(p) for p in (nav/'migrations').glob('*.sql')}==m['nav_migrations']
for name,value in m['nav_binaries'].items(): assert sha(stage/'nav'/name)==value
for name,value in m['frontend'].items(): assert sha(stage/'frontend'/name)==value
for scope,unit in [([], 'crypto-nav-manager.service'),(['--user'],'crypto-cta-manager-web.service')]:
    subprocess.run(['systemctl',*scope,'--quiet','is-active',unit],check=True)
backup=root/'backups'/RELEASE
assert not backup.exists()
(backup/'nav').mkdir(parents=True,mode=0o700)
previous_webroot=os.readlink(root/'webroot')
os.link(root/'bin/cta_web',backup/'cta_web.previous')
for name in m['nav_binaries']: os.link(nav/'target/release'/name,backup/'nav'/name)
state={'previous_webroot':previous_webroot,'nav_previous_commit':m['nav_previous_commit'],
       'cta_sha256':sha(root/'bin/cta_web'),
       'nav_binaries':{name:sha(nav/'target/release'/name) for name in m['nav_binaries']}}
(backup/'rollback.json').write_text(json.dumps(state,indent=2)+'\n')
subprocess.run(['git','fetch','origin'],cwd=nav,check=True,stdout=subprocess.DEVNULL)
assert subprocess.check_output(['git','rev-parse','origin/master'],cwd=nav,text=True).strip()==m['nav_commit']
subprocess.run(['git','switch','master'],cwd=nav,check=True,stdout=subprocess.DEVNULL)
subprocess.run(['git','merge','--ff-only','origin/master'],cwd=nav,check=True,stdout=subprocess.DEVNULL)
web=root/'web-releases'/RELEASE
assert not web.exists()
(web/'manager').mkdir(parents=True)
shutil.copytree(root/'webroot/manager/assets',web/'manager/assets')
shutil.copytree(stage/'frontend',web/'manager',dirs_exist_ok=True)
shutil.copy2(stage/'manifest.json',web/'RELEASE-MANIFEST.json')
link=root/('webroot.next.'+RELEASE)
link.symlink_to('web-releases/'+RELEASE)
os.chmod(stage/'cta_web',0o755)
os.replace(stage/'cta_web',root/'bin/cta_web')
os.replace(link,root/'webroot')
subprocess.run(['systemctl','--user','restart','crypto-cta-manager-web.service'],check=True)
subprocess.run(['systemctl','--user','--quiet','is-active','crypto-cta-manager-web.service'],check=True)
for name in m['nav_binaries']:
    os.chmod(stage/'nav'/name,0o755)
    os.replace(stage/'nav'/name,nav/'target/release'/name)
subprocess.run(['sudo','-n','systemctl','restart','crypto-nav-manager.service'],check=True)
subprocess.run(['systemctl','--quiet','is-active','crypto-nav-manager.service'],check=True)
print(json.dumps({'release':RELEASE,'backup':str(backup),'previous_webroot':previous_webroot,
                  'cta_sha256':sha(root/'bin/cta_web'),'nav_sha256':sha(nav/'target/release/crypto_nav_manager')}),flush=True)
'''
r = subprocess.run(['ssh', 'jp-meta-elvpn', 'python3', '-'],
                   input='RELEASE=' + repr(RELEASE) + '\n' + remote, text=True, capture_output=True)
(OUT / 'deployment.json').write_text(r.stdout)
if r.returncode:
    print(r.stderr)
    raise SystemExit(r.returncode)
print(r.stdout, end='')
