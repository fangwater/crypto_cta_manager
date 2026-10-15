"""Publish only the backed-up Manager backend and frontend; preserve live units."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time
import urllib.request

ROOT = Path('/home/el01/crypto_cta_manager')
RELEASE = '20261014T093305Z'
STAGE = ROOT / 'deploy-staging' / ('position-creation-' + RELEASE)
BACKUP = ROOT / 'backups' / ('position-creation-' + RELEASE)
manifest = json.loads((STAGE / 'manifest.json').read_text())
before = json.loads((STAGE / 'before_state.json').read_text())
assert (BACKUP / 'cta_web').is_file() and (BACKUP / 'manager.pgdump').stat().st_size > 0
assert hashlib.sha256((ROOT / 'bin/cta_web').read_bytes()).hexdigest() == manifest['previous_cta_web_sha256']
assert str((ROOT / 'webroot').resolve()) == before['webroot']
dependencies = subprocess.check_output([
    'systemctl', '--user', 'show', 'crypto-cta-nginx.service',
    '--property=Requires,BindsTo,PartOf', '--value',
], text=True)
assert 'crypto-cta-manager-web.service' not in dependencies
for name, expected in manifest['sha256'].items():
    assert hashlib.sha256((STAGE / name).read_bytes()).hexdigest() == expected, 'staged artifact checksum mismatch'

release_root = ROOT / 'web-releases' / RELEASE
if release_root.exists():
    for name, expected in manifest['sha256'].items():
        if name.startswith('frontend/'):
            assert hashlib.sha256((release_root / name.removeprefix('frontend/')).read_bytes()).hexdigest() == expected
else:
    shutil.copytree(STAGE / 'frontend', release_root)
shutil.copy2(STAGE / 'manifest.json', release_root / 'RELEASE-MANIFEST.json')
shutil.copy2(STAGE / 'source.patch', release_root / 'source.patch')
binary_next = ROOT / 'bin' / ('cta_web.next.' + RELEASE)
shutil.copy2(STAGE / 'cta_web', binary_next)
os.chmod(binary_next, 0o755)
os.replace(binary_next, ROOT / 'bin/cta_web')
link_next = ROOT / ('webroot.next.' + RELEASE)
os.symlink('web-releases/' + RELEASE, link_next)
os.replace(link_next, ROOT / 'webroot')

def restart_and_wait():
    subprocess.run(['systemctl', '--user', 'restart', 'crypto-cta-manager-web.service'],
                   check=True, capture_output=True, timeout=120)
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    for _ in range(600):
        try:
            with opener.open('http://127.0.0.1:18201/api/auth/status', timeout=5) as response:
                if response.status == 200:
                    return
        except Exception:
            pass
        time.sleep(1)
    raise RuntimeError('Manager did not become ready')

try:
    restart_and_wait()
except Exception:
    shutil.copy2(BACKUP / 'cta_web', binary_next)
    os.replace(binary_next, ROOT / 'bin/cta_web')
    os.symlink((BACKUP / 'webroot.txt').read_text().strip(), link_next)
    os.replace(link_next, ROOT / 'webroot')
    restart_and_wait()
    raise RuntimeError('Manager publication failed; previous backend/frontend restored')

assert hashlib.sha256((ROOT / 'bin/cta_web').read_bytes()).hexdigest() == manifest['sha256']['cta_web']
for name, expected in manifest['sha256'].items():
    if name.startswith('frontend/'):
        assert hashlib.sha256((ROOT / 'webroot' / name.removeprefix('frontend/')).read_bytes()).hexdigest() == expected
record = ROOT / 'RUNTIME-RELEASE.txt'
previous = record.read_text() if record.is_file() else ''
updates = {
    'cta_web_sha256': manifest['sha256']['cta_web'],
    'cta_web_source_commit': manifest['git_head'],
    'frontend_source_commit': manifest['git_head'],
    'frontend_index_sha256': manifest['sha256']['frontend/manager/index.html'],
    'release': 'web-releases/' + RELEASE,
    'position_creation_patch_sha256': manifest['source_patch_sha256'],
    'position_creation_manifest': str(release_root / 'RELEASE-MANIFEST.json'),
}
lines = [line for line in previous.splitlines() if line.split('=', 1)[0] not in updates]
lines.extend(key + '=' + value for key, value in updates.items())
record.write_text('\n'.join(lines) + '\n')
print(json.dumps({'release': RELEASE, 'manager_ready': True,
                  'backend_sha256': manifest['sha256']['cta_web'],
                  'live_frontend_sha256_verified': True,
                  'service_units_preserved': True}), flush=True)
