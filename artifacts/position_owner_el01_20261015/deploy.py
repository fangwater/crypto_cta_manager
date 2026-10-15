"""Checksum and publish Manager only, with rollback and protected-process checks."""
from pathlib import Path
import hashlib
import json
import subprocess

artifact = Path(__file__).resolve().parent
repo = artifact.parents[1]
remote_root = '/home/el01/crypto_cta_manager'
release = '20261015T040000Z'
stage = remote_root + '/deploy-staging/position-owner-' + release
binary = Path('/mnt/nvme-raid0-28t/fanghaizhou/cargo-target/crypto_cta_manager/release/cta_web')
dist = repo / 'frontend/dist'
backup = json.loads((artifact / 'backup.json').read_text())
sources = sorted((repo / 'src').rglob('*.rs')) + sorted((repo / 'frontend/src').rglob('*'))
sources += [repo / 'Cargo.toml', repo / 'Cargo.lock']
source_hashes = {str(p.relative_to(repo)): hashlib.sha256(p.read_bytes()).hexdigest()
                 for p in sources if p.is_file()}
hashes = {'cta_web': hashlib.sha256(binary.read_bytes()).hexdigest()}
hashes.update({'frontend/manager/' + str(p.relative_to(dist)): hashlib.sha256(p.read_bytes()).hexdigest()
               for p in sorted(dist.rglob('*')) if p.is_file()})
patch = subprocess.check_output(['git', 'diff', '--binary', '--', 'src', 'frontend'], cwd=repo)
(artifact / 'source.patch').write_bytes(patch)
manifest = {'release': release, 'git_head': subprocess.check_output(['git', 'rev-parse', 'HEAD'],
             cwd=repo, text=True).strip(), 'sha256': hashes, 'source_hashes': source_hashes,
            'previous_cta_web_sha256': backup['previous_cta_web_sha256'],
            'previous_webroot': backup['webroot'], 'source_patch_sha256': hashlib.sha256(patch).hexdigest(),
            'change': 'Creator implicitly holds strategy configure access, including grant management; account access remains independently required.'}
(artifact / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
subprocess.run(['ssh', 'cta_exec', 'mkdir', '-p', stage + '/frontend/manager'], check=True)
subprocess.run(['rsync', '-a', str(binary), 'cta_exec:' + stage + '/cta_web'], check=True)
subprocess.run(['rsync', '-a', str(dist) + '/', 'cta_exec:' + stage + '/frontend/manager/'], check=True)
subprocess.run(['scp', str(artifact / 'manifest.json'), str(artifact / 'source.patch'), 'cta_exec:' + stage + '/'], check=True)
assert all(hashlib.sha256((repo / name).read_bytes()).hexdigest() == digest
           for name, digest in source_hashes.items()), 'source changed during publication preparation'
script = (artifact / 'common_remote.py').read_text() + r'''
import time
manifest=json.loads((STAGE/'manifest.json').read_text())
before=json.loads((STAGE/'before_state.json').read_text())
assert hashlib.sha256((ROOT/'bin/cta_web').read_bytes()).hexdigest()==manifest['previous_cta_web_sha256']
assert str((ROOT/'webroot').resolve())==manifest['previous_webroot']
current=capture_state()
assert current==before, 'runtime changed before deployment'
dependencies=subprocess.check_output(['systemctl','--user','show','crypto-cta-nginx.service',
    '--property=Requires,BindsTo,PartOf','--value'],text=True)
assert 'crypto-cta-manager-web.service' not in dependencies
for name,digest in manifest['sha256'].items():
    assert hashlib.sha256((STAGE/name).read_bytes()).hexdigest()==digest,name
release_root=ROOT/'web-releases'/RELEASE
assert not release_root.exists()
shutil.copytree(STAGE/'frontend',release_root)
shutil.copy2(STAGE/'manifest.json',release_root/'RELEASE-MANIFEST.json')
shutil.copy2(STAGE/'source.patch',release_root/'source.patch')
binary_next=ROOT/'bin'/('cta_web.next.owner-'+RELEASE)
link_next=ROOT/('webroot.next.owner-'+RELEASE)
assert not binary_next.exists() and not link_next.exists() and not link_next.is_symlink()
shutil.copy2(STAGE/'cta_web',binary_next)
os.chmod(binary_next,0o755)
os.replace(binary_next,ROOT/'bin/cta_web')
os.symlink('web-releases/'+RELEASE,link_next)
os.replace(link_next,ROOT/'webroot')
opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))

def ready():
    subprocess.run(['systemctl','--user','restart','crypto-cta-manager-web.service'],
                   check=True,capture_output=True,timeout=120)
    began=time.monotonic()
    while time.monotonic()-began<600:
        try:
            with opener.open('http://127.0.0.1:10051/manager/api/auth/status',timeout=5) as response:
                if response.status==200:
                    return
        except Exception:
            pass
        time.sleep(1)
    raise RuntimeError('Manager did not become ready within 600 seconds')

try:
    print(json.dumps({'phase':'manager_restart','release':RELEASE}),flush=True)
    ready()
    for name,digest in manifest['sha256'].items():
        path=ROOT/'bin/cta_web' if name=='cta_web' else release_root/name.removeprefix('frontend/')
        assert hashlib.sha256(path.read_bytes()).hexdigest()==digest,name
        if name.startswith('frontend/manager/'):
            suffix=name.removeprefix('frontend/manager/')
            url='http://127.0.0.1:10051/manager/'+('' if suffix=='index.html' else suffix)
            with opener.open(url,timeout=10) as response:
                assert response.status==200 and hashlib.sha256(response.read()).hexdigest()==digest,name
    after=capture_state()
    assert before['protected_processes']==after['protected_processes'], 'protected processes changed'
    assert before['config_sha256']==after['config_sha256'], 'protected configuration changed'
    assert before['manager_pid']!=after['manager_pid']
except Exception:
    shutil.copy2(BACKUP/'cta_web',binary_next)
    os.replace(binary_next,ROOT/'bin/cta_web')
    os.symlink((BACKUP/'webroot.txt').read_text().strip(),link_next)
    os.replace(link_next,ROOT/'webroot')
    ready()
    raise RuntimeError('publication verification failed; previous Manager backend and frontend restored')
(STAGE/'after_state.json').write_text(json.dumps(after,indent=2)+'\n')
record=ROOT/'RUNTIME-RELEASE.txt'
updates={'cta_web_sha256':manifest['sha256']['cta_web'], 'cta_web_source_commit':manifest['git_head'],
         'frontend_source_commit':manifest['git_head'],'release':'web-releases/'+RELEASE,
         'frontend_index_sha256':manifest['sha256']['frontend/manager/index.html'],
         'position_owner_manifest':str(release_root/'RELEASE-MANIFEST.json')}
lines=[line for line in record.read_text().splitlines() if line.split('=',1)[0] not in updates]
record.write_text('\n'.join(lines+[key+'='+value for key,value in updates.items()])+'\n')
print(json.dumps({'phase':'deployed','release':RELEASE,'manager_ready':True,'manager_pid':after['manager_pid'],
                  'protected_processes_unchanged':len(before['protected_processes']),
                  'trade_configs_unchanged':before['trading_config_count'],
                  'backend_sha256':manifest['sha256']['cta_web']}),flush=True)
'''
result = subprocess.run(['ssh', '-o', 'BatchMode=yes', 'cta_exec', 'python3', '-'], input=script,
                        text=True, capture_output=True)
(artifact / 'deployment.jsonl').write_text(result.stdout)
print(result.stdout, end='', flush=True)
if result.returncode:
    raise RuntimeError(result.stderr)
