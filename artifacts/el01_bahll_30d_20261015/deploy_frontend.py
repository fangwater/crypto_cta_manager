"""Publish the checked frontend without restarting any runtime service."""
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import subprocess

artifact = Path(__file__).resolve().parent
repo = artifact.parents[1]
dist = repo / 'frontend/dist'
stamp = datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ')
release = 'web-releases/' + stamp
remote_root = '/home/el01/crypto_cta_manager'
snapshot_source = (repo / 'artifacts/funding_compact_el01_20261014/runtime_snapshot_remote.py').read_text()
snapshot_source += "\nresult=runtime_snapshot()\nresult['webroot']=os.readlink(root/'webroot')\nprint(json.dumps(result))\n"


def remote(script):
    result = subprocess.run(['ssh', '-o', 'BatchMode=yes', 'cta_exec', 'python3', '-'],
                            input=script, text=True, capture_output=True)
    if result.returncode:
        raise RuntimeError(result.stderr)
    return json.loads(result.stdout)


before = remote(snapshot_source)
(artifact / 'before_deployment.json').write_text(json.dumps(before, indent=2) + '\n')
hashes = {str(path.relative_to(dist)): hashlib.sha256(path.read_bytes()).hexdigest()
          for path in sorted(dist.rglob('*')) if path.is_file()}
assert 'index.html' in hashes and len(hashes) >= 3
source_hashes = {str(path.relative_to(repo)): hashlib.sha256(path.read_bytes()).hexdigest()
                 for path in sorted((repo / 'frontend/src').rglob('*')) if path.is_file()}
manifest = {'release': release, 'previous_frontend_release': before['webroot'],
            'frontend_hashes': hashes, 'source_hashes': source_hashes,
            'git_head': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=repo, text=True).strip(),
            'runtime_binary_sha256': before['runtime_sha256'],
            'change': 'Align the 30D NAV shortcut to server candle availability for both factual and theoretical queries.'}
subprocess.run(['ssh', '-o', 'BatchMode=yes', 'cta_exec', 'mkdir', '-m0755', remote_root + '/' + release], check=True)
subprocess.run(['rsync', '-a', str(dist) + '/', 'cta_exec:' + remote_root + '/' + release + '/manager/'], check=True)

script = snapshot_source[:snapshot_source.index('\nresult=runtime_snapshot()')]
script += '\nbefore=' + repr(before) + '\nmanifest=' + repr(manifest) + '\n'
script += r'''
import urllib.request
assert runtime_snapshot()=={key:value for key,value in before.items() if key!='webroot'}, 'runtime changed before publish'
assert os.readlink(root/'webroot')==before['webroot'], 'concurrent frontend release'
for name,digest in manifest['frontend_hashes'].items():
    assert hashlib.sha256((root/manifest['release']/'manager'/name).read_bytes()).hexdigest()==digest, name
(root/manifest['release']/'RELEASE-MANIFEST.json').write_text(json.dumps(manifest,indent=2)+'\n')
next_link=root/('webroot.next.nav-30d-'+Path(manifest['release']).name)
assert not next_link.exists() and not next_link.is_symlink()
os.symlink(manifest['release'],next_link)
os.replace(next_link,root/'webroot')
try:
    opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
    for name,digest in manifest['frontend_hashes'].items():
        path='/manager/' if name=='index.html' else '/manager/'+name
        with opener.open('http://127.0.0.1:10051'+path,timeout=10) as response:
            assert response.status==200
            assert hashlib.sha256(response.read()).hexdigest()==digest,name
    after=runtime_snapshot()
    assert after=={key:value for key,value in before.items() if key!='webroot'}, 'runtime changed after publish'
except Exception:
    if os.readlink(root/'webroot')==manifest['release']:
        os.symlink(before['webroot'],next_link)
        os.replace(next_link,root/'webroot')
    raise
print(json.dumps({'release':manifest['release'],'previous_release':before['webroot'],
                  'protected_processes_unchanged':len(before['processes']),
                  'trade_configs_unchanged':len(before['trade_configs']),
                  'runtime_and_config_unchanged':True,'manager_restarted':False,
                  'frontend_hashes_verified':manifest['frontend_hashes']}))
'''
record = remote(script)
(artifact / 'deployment.json').write_text(json.dumps(record, indent=2) + '\n')
(artifact / 'release_manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
print(json.dumps(record))
