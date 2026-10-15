"""Upload checked static files and atomically swap el01's frontend symlink."""
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import subprocess

repo = Path(__file__).resolve().parents[2]
dist = repo / 'frontend/dist'
artifact = Path(__file__).resolve().parent
release = 'web-releases/' + datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ')
remote_root = '/home/el01/crypto_cta_manager'
hashes = {str(path.relative_to(dist)): hashlib.sha256(path.read_bytes()).hexdigest()
          for path in sorted(dist.rglob('*')) if path.is_file()}
assert 'index.html' in hashes and len(hashes) >= 3
subprocess.run(['ssh', '-o', 'BatchMode=yes', 'cta_exec',
                'install', '-d', '-m0755', remote_root + '/' + release + '/manager'], check=True)
subprocess.run(['rsync', '-a', '--delete', str(dist) + '/',
                'cta_exec:' + remote_root + '/' + release + '/manager/'], check=True)
script = 'release = ' + repr(release) + '\nhashes = ' + repr(hashes) + '\n' + r'''
import hashlib
import json
import os
from pathlib import Path
import subprocess
root = Path('/home/el01/crypto_cta_manager')
unit = 'crypto-cta-manager-web.service'
def pid():
    return subprocess.check_output(['systemctl', '--user', 'show', unit, '--property=MainPID', '--value'], text=True).strip()
before = pid()
assert before != '0'
old_release = os.readlink(root / 'webroot')
assert (root / old_release / 'manager/index.html').is_file()
for name, expected in hashes.items():
    assert hashlib.sha256((root / release / 'manager' / name).read_bytes()).hexdigest() == expected, name
manifest = {'frontend_release': release, 'previous_frontend_release': old_release,
            'frontend_hashes': hashes, 'runtime_binary_sha256': hashlib.sha256((root / 'bin/cta_web').read_bytes()).hexdigest(),
            'manager_pid': before}
(root / release / 'RELEASE-MANIFEST.json').write_text(json.dumps(manifest, indent=2) + '\n')
next_link = root / 'webroot.next'
assert not next_link.exists() and not next_link.is_symlink(), 'another frontend deployment is pending'
os.symlink(release, next_link)
os.replace(next_link, root / 'webroot')
assert pid() == before, 'Manager PID changed'
assert not next_link.exists() and not next_link.is_symlink()
print(json.dumps({'kind': 'frontend_release', 'release': release, 'previous_release': old_release,
                  'files_verified': len(hashes), 'manager_pid_unchanged': True,
                  'runtime_binary_sha256': manifest['runtime_binary_sha256']}))
'''
result = subprocess.run(['ssh', '-o', 'BatchMode=yes', 'cta_exec', 'python3', '-'],
                        input=script, text=True, capture_output=True)
if result.returncode:
    print(result.stderr)
    raise SystemExit(result.returncode)
record = json.loads(result.stdout)
(artifact / 'deployment.json').write_text(json.dumps(record, indent=2) + '\n')
print(result.stdout, end='')
