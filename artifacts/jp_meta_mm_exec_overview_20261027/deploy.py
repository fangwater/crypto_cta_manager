"""Stage and deploy the explicit jp-meta aggregation and SG read-only export."""
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import secrets
import subprocess

OUT = Path(__file__).resolve().parent
CTA = OUT.parent.parent
NAV = Path('/home/fanghaizhou/crypto_nav_manager')
RELEASE = 'mm-exec-overview-' + datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ')
ROOT = '/home/ubuntu/crypto_cta_manager'
STAGE = ROOT + '/releases/' + RELEASE
TARGET = Path('/mnt/nvme-raid0-28t/fanghaizhou/cargo-target')
BINS = ['crypto_nav_manager', 'reconcile_rocksdb', 'sync_history', 'sync_fee_rates',
        'sync_intra_orders', 'sync_intra_hourly_latency', 'fee_rate_probe']
sha = lambda p: hashlib.sha256(p.read_bytes()).hexdigest()
before = {}
for host in ['sg', 'jp-meta-elvpn']:
    rows = [json.loads(line) for line in (OUT / (host + '-before.jsonl')).read_text().splitlines()]
    before[host] = next(row['body'] for row in rows if row['name'] == 'protected')
m = {
    'release': RELEASE,
    'cta_commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=CTA, text=True).strip(),
    'nav_commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=NAV, text=True).strip(),
    'nav_previous_commit': subprocess.check_output(['git', 'rev-parse', 'HEAD^'], cwd=NAV, text=True).strip(),
    'cta_sha256': sha(TARGET / 'crypto_cta_manager/release/cta_web'),
    'before': {host: {'binary': state['binary_sha256'], 'config': state['config_hashes'][ROOT + '/config/cta-manager.toml']}
               for host, state in before.items()},
    'nav_binaries': {name: sha(TARGET / 'crypto_nav_manager/release' / name) for name in BINS},
    'nav_migrations': {p.name: sha(p) for p in (NAV / 'migrations').glob('*.sql')},
    'frontend': {str(p.relative_to(CTA / 'frontend/dist')): sha(p)
                 for p in (CTA / 'frontend/dist').rglob('*') if p.is_file()},
}
(OUT / 'manifest.json').write_text(json.dumps(m, indent=2) + '\n')
maintenance = '\n'.join((CTA / 'scripts' / name).read_text() for name in [
    'apply_grid_order_schema.sql', 'apply_order_strategy_ownership.sql', 'apply_allocation_schema.sql'])
# The token is transported over SSH stdin and saved only in mode-0600 host files.
token = secrets.token_urlsafe(48)
common = (OUT / 'host_common.py').read_text()
remote = r'''
import re,shutil
root=ROOT
sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
assert subprocess.check_output(['whoami'],text=True).strip()=='ubuntu'
expected={'sg':'ip-172-31-7-123.ap-southeast-1.compute.internal','jp-meta-elvpn':'ip-172-31-35-228.ap-northeast-1.compute.internal'}
assert subprocess.check_output(['hostname','-f'],text=True).strip()==expected[HOST]
assert '2.39' in subprocess.check_output(['ldd','--version'],text=True).splitlines()[0]
stage=root/'releases'/M['release']
config=root/'config/cta-manager.toml'
assert sha(config)==M['before'][HOST]['config']
assert sha(root/'bin/cta_web')==M['before'][HOST]['binary']
assert sha(stage/'cta_web')==M['cta_sha256']
backup=root/'backups'/M['release']
assert not backup.exists()
backup.mkdir(mode=0o700,parents=True)
os.link(root/'bin/cta_web',backup/'cta_web.previous')
shutil.copy2(config,backup/'cta-manager.toml.previous')
previous=os.readlink(root/'webroot')
(backup/'rollback.json').write_text(json.dumps({'webroot':previous,'nav_commit':M['nav_previous_commit']})+'\n')
if HOST=='sg':
    os.umask(0o077)
    subprocess.run(['pg_dump','-Fc','-f',str(backup/'database-before.dump')],env=db_env,check=True,capture_output=True)
    assert sql("SELECT to_regclass('cta_strategy_allocation_snapshots') IS NULL;")=='t'
    sql(MAINTENANCE,False)
    old=config.read_text()
    assert 'overview_export' not in cfg['dashboard']
    new=old+'\n[dashboard.overview_export]\nenabled = true\ntoken_env = "CRYPTO_SG_OVERVIEW_TOKEN"\nsource_ids = ["bybit_exec_trade01"]\n'
else:
    nav=Path('/home/ubuntu/crypto_nav_manager')
    assert not subprocess.check_output(['git','status','--porcelain'],cwd=nav,text=True).strip()
    assert subprocess.check_output(['git','rev-parse','HEAD'],cwd=nav,text=True).strip()==M['nav_previous_commit']
    assert {p.name:sha(p) for p in (nav/'migrations').glob('*.sql')}==M['nav_migrations']
    (backup/'nav').mkdir()
    for name,value in M['nav_binaries'].items():
        assert sha(stage/'nav'/name)==value
        os.link(nav/'target/release'/name,backup/'nav'/name)
    subprocess.run(['git','fetch','origin'],cwd=nav,check=True,stdout=subprocess.DEVNULL)
    assert subprocess.check_output(['git','rev-parse','origin/master'],cwd=nav,text=True).strip()==M['nav_commit']
    subprocess.run(['git','switch','master'],cwd=nav,check=True,stdout=subprocess.DEVNULL)
    subprocess.run(['git','merge','--ff-only','origin/master'],cwd=nav,check=True,stdout=subprocess.DEVNULL)
    old=config.read_text()
    assert not cfg['dashboard'].get('external_overviews')
    assert not cfg['dashboard']['nav_overview'].get('viewer_usernames')
    new,count=re.subn(r'(?m)^\[dashboard\.nav_overview\]\s*$', '[dashboard.nav_overview]\nviewer_usernames = ["lgq"]',old)
    assert count==1
    new+='\n[[dashboard.external_overviews]]\nenabled = true\nprovider_id = "sg-cta"\nurl = "http://47.131.162.78:4191/manager/api/integrations/account-overview"\ndetail_base_url = "http://47.131.162.78:4191/manager/nav/?source="\ntoken_env = "CRYPTO_SG_OVERVIEW_TOKEN"\nviewer_usernames = ["lgq"]\nrefresh_secs = 15\nrequest_timeout_secs = 10\nstale_secs = 90\n'
    web=root/'web-releases'/M['release']
    (web/'manager').mkdir(parents=True)
    shutil.copytree(root/'webroot/manager/assets',web/'manager/assets')
    for name,value in M['frontend'].items():assert sha(stage/'frontend'/name)==value
    shutil.copytree(stage/'frontend',web/'manager',dirs_exist_ok=True)
    shutil.copy2(stage/'manifest.json',web/'RELEASE-MANIFEST.json')
    link=root/('webroot.next.'+M['release'])
    link.symlink_to('web-releases/'+M['release'])
    os.replace(link,root/'webroot')
parsed=tomllib.loads(new)
assert parsed['sources']==cfg['sources']
for key in cfg:
    if key!='dashboard':assert parsed[key]==cfg[key]
env_dir=Path('/home/ubuntu/.config/crypto-manager-integration')
env_dir.mkdir(mode=0o700,parents=True,exist_ok=True)
secret_path=env_dir/(M['release']+'.env')
assert not secret_path.exists()
fd=os.open(secret_path,os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600)
with os.fdopen(fd,'w') as f:f.write('CRYPTO_SG_OVERVIEW_TOKEN='+TOKEN+'\n')
dropin=Path('/home/ubuntu/.config/systemd/user/crypto-cta-manager-web.service.d')
dropin.mkdir(parents=True,exist_ok=True)
unit_file=dropin/'40-account-overview.conf'
assert not unit_file.exists()
unit_file.write_text('[Service]\nEnvironmentFile='+str(secret_path)+'\n')
temp=config.with_name('cta-manager.toml.next.'+M['release'])
temp.write_text(new)
os.chmod(temp,config.stat().st_mode&0o777)
os.replace(temp,config)
os.chmod(stage/'cta_web',0o755)
os.replace(stage/'cta_web',root/'bin/cta_web')
subprocess.run(['systemctl','--user','daemon-reload'],check=True)
subprocess.run(['systemctl','--user','restart',UNIT],check=True)
subprocess.run(['systemctl','--user','--quiet','is-active',UNIT],check=True)
if HOST=='jp-meta-elvpn':
    for name in M['nav_binaries']:
        os.chmod(stage/'nav'/name,0o755)
        os.replace(stage/'nav'/name,nav/'target/release'/name)
    subprocess.run(['sudo','-n','systemctl','restart','crypto-nav-manager.service'],check=True)
    subprocess.run(['systemctl','--quiet','is-active','crypto-nav-manager.service'],check=True)
emit('deployed',{'host':HOST,'release':M['release'],'backup':str(backup),'config_sha256':sha(config)})
'''
for host in ['sg', 'jp-meta-elvpn']:
    subprocess.run(['ssh', host, 'install', '-d', '-m', '0700', STAGE + '/nav'], check=True)
    subprocess.run(['scp', '-q', str(TARGET / 'crypto_cta_manager/release/cta_web'), str(OUT / 'manifest.json'), host + ':' + STAGE + '/'], check=True)
    if host == 'jp-meta-elvpn':
        subprocess.run(['scp', '-q', *[str(TARGET / 'crypto_nav_manager/release' / name) for name in BINS], host + ':' + STAGE + '/nav/'], check=True)
        subprocess.run(['rsync', '-a', str(CTA / 'frontend/dist') + '/', host + ':' + STAGE + '/frontend/'], check=True)
    script = common + '\nM=' + repr(m) + '\nHOST=' + repr(host) + '\nTOKEN=' + repr(token) + '\nMAINTENANCE=' + repr(maintenance) + '\n' + remote
    r = subprocess.run(['ssh', host, 'python3', '-'], input=script, text=True, capture_output=True)
    (OUT / (host + '-deployment.jsonl')).write_text(r.stdout)
    if r.returncode:
        print(r.stderr)
        raise SystemExit(r.returncode)
    print(r.stdout, end='', flush=True)
