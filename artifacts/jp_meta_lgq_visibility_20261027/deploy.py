"""Deploy the jp-meta-only visibility cap and temporary overview exclusions."""
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import subprocess

OUT = Path(__file__).resolve().parent
ROOT = '/home/ubuntu/crypto_cta_manager'
BINS = ['cta_web', 'nav_rebuild', 'nav_snapshot', 'nav_strategy_snapshot', 'cta_monitor']
BUILD = Path('/mnt/nvme-raid0-28t/fanghaizhou/cargo-target/crypto_cta_manager/release')
RELEASE = 'lgq-visibility-' + datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ')
private = ['binance_exec_trade06', 'binance_exec_trade07', 'binance_exec_trade08', 'binance_exec_rx04_5m05']
previous = next(json.loads(line)['body'] for line in (OUT / 'before.jsonl').read_text().splitlines() if json.loads(line)['name'] == 'protected')
m = {'release': RELEASE, 'commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip(),
     'binaries': {name: hashlib.sha256((BUILD / name).read_bytes()).hexdigest() for name in BINS},
     'before': {'binary': previous['binary_sha256'], 'config': previous['config_hashes'][ROOT + '/config/cta-manager.toml']},
     'source_viewers': {name: ['lgq'] for name in private}, 'excluded': ['gate_fr_arb04', 'gate_fr_arb05']}
(OUT / 'manifest.json').write_text(json.dumps(m, indent=2) + '\n')
stage = ROOT + '/releases/' + RELEASE
subprocess.run(['ssh', 'jp-meta-elvpn', 'install', '-d', '-m', '0700', stage], check=True)
for name in BINS:
    subprocess.run(['scp', '-q', str(BUILD / name), 'jp-meta-elvpn:' + stage + '/' + name + '.next'], check=True)
subprocess.run(['scp', '-q', str(OUT / 'manifest.json'), 'jp-meta-elvpn:' + stage + '/'], check=True)
common = (OUT.parent / 'jp_meta_mm_exec_overview_20261027/host_common.py').read_text()
remote = r'''
import copy,re,shutil
assert subprocess.check_output(['hostname','-f'],text=True).strip()=='ip-172-31-35-228.ap-northeast-1.compute.internal'
assert subprocess.check_output(['whoami'],text=True).strip()=='ubuntu'
sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
config=ROOT/'config/cta-manager.toml'
assert sha(config)==M['before']['config']
assert sha(ROOT/'bin/cta_web')==M['before']['binary']
assert not cfg['dashboard'].get('source_viewers')
assert not cfg['dashboard']['nav_overview'].get('excluded_source_ids')
assert set(M['source_viewers']) <= {s['id'] for s in cfg['sources'] if s['enabled']}
stage=ROOT/'releases'/M['release']
for name,value in M['binaries'].items():assert sha(stage/(name+'.next'))==value
backup=ROOT/'backups'/M['release']
backup.mkdir(mode=0o700,parents=True,exist_ok=False)
shutil.copy2(config,backup/'cta-manager.toml.previous')
for name in M['binaries']:os.link(ROOT/'bin'/name,backup/(name+'.previous'))
old=config.read_text()
new,count=re.subn(r'(?m)^\[dashboard\.nav_overview\]\s*$', '[dashboard.nav_overview]\nexcluded_source_ids = ["gate_fr_arb04", "gate_fr_arb05"]',old)
assert count==1
new+='\n[dashboard.source_viewers]\n'+''.join(name+' = ["lgq"]\n' for name in M['source_viewers'])
parsed=tomllib.loads(new)
expected=copy.deepcopy(cfg)
expected['dashboard']['source_viewers']=M['source_viewers']
expected['dashboard']['nav_overview']['excluded_source_ids']=M['excluded']
assert parsed==expected
next_config=config.with_name('cta-manager.toml.next.'+M['release'])
next_config.write_text(new)
os.chmod(next_config,config.stat().st_mode&0o777)
os.replace(next_config,config)
for name in M['binaries']:
    os.chmod(stage/(name+'.next'),0o755)
    os.replace(stage/(name+'.next'),ROOT/'bin'/name)
subprocess.run(['systemctl','--user','restart',UNIT],check=True)
subprocess.run(['systemctl','--user','--quiet','is-active',UNIT],check=True)
emit('deployed',{'release':M['release'],'config_sha256':sha(config),'backup':str(backup),'webroot':str((ROOT/'webroot').resolve())})
'''
r = subprocess.run(['ssh', 'jp-meta-elvpn', 'python3 -'], input=common + '\nM=' + repr(m) + '\n' + remote, text=True, capture_output=True)
(OUT / 'deployment.jsonl').write_text(r.stdout)
print(r.stdout, end='')
if r.returncode:
    print(r.stderr)
    raise SystemExit(r.returncode)
