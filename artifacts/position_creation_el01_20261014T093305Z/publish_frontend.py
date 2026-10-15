import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess

ROOT = Path('/home/el01/crypto_cta_manager')
STAGE = ROOT / 'deploy-staging/position-creation-20261014T093305Z'
manifest = json.loads((STAGE / 'frontend_manifest.json').read_text())
assert hashlib.sha256((ROOT / 'bin/cta_web').read_bytes()).hexdigest() == manifest['cta_web_sha256']
before_pid = subprocess.check_output(['systemctl', '--user', 'show',
                                     'crypto-cta-manager-web.service', '-p', 'MainPID', '--value'])
assert (ROOT / 'webroot').resolve().name == manifest['backend_release']
for name, expected in manifest['sha256'].items():
    assert hashlib.sha256((STAGE / 'frontend-fix' / name).read_bytes()).hexdigest() == expected
release = ROOT / 'web-releases' / manifest['frontend_release']
shutil.copytree(STAGE / 'frontend-fix', release)
shutil.copy2(STAGE / 'frontend_manifest.json', release / 'RELEASE-MANIFEST.json')
shutil.copy2(STAGE / 'frontend_source.patch', release / 'source.patch')
link_next = ROOT / ('webroot.next.' + manifest['frontend_release'])
os.symlink('web-releases/' + manifest['frontend_release'], link_next)
os.replace(link_next, ROOT / 'webroot')
assert subprocess.check_output(['systemctl', '--user', 'show',
                                'crypto-cta-manager-web.service', '-p', 'MainPID', '--value']) == before_pid
for name, expected in manifest['sha256'].items():
    assert hashlib.sha256((ROOT / 'webroot' / name).read_bytes()).hexdigest() == expected
record = ROOT / 'RUNTIME-RELEASE.txt'
updates = {'release': 'web-releases/' + manifest['frontend_release'],
           'frontend_index_sha256': manifest['sha256']['manager/index.html'],
           'position_creation_frontend_manifest': str(release / 'RELEASE-MANIFEST.json'),
           'position_creation_frontend_patch_sha256': manifest['source_patch_sha256']}
lines = [line for line in record.read_text().splitlines() if line.split('=', 1)[0] not in updates]
lines += [key + '=' + value for key, value in updates.items()]
record.write_text('\n'.join(lines) + '\n')
print(json.dumps({'frontend_release': manifest['frontend_release'],
                  'checksums_verified': True, 'manager_pid_unchanged': True}))
