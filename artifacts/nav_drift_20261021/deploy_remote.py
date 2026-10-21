import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess

root = Path('/home/el01/crypto_cta_manager')
release = '20261021T065251Z'
stage = root / 'releases' / release
manifest = json.loads((stage / 'manifest.json').read_text())
sha = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
assert manifest['release'] == release
assert sha(stage / 'cta_web') == manifest['binary_sha256']
for name, expected in manifest['frontend'].items():
    assert sha(stage / 'frontend' / name) == expected, name
unit = 'crypto-cta-manager-web.service'
pid = subprocess.check_output(['systemctl','--user','show',unit,'-p','MainPID','--value'],text=True).strip()
assert str(Path('/proc',pid,'exe').resolve()) == str(root / 'bin/cta_web')
previous_web = (root / 'webroot').resolve()
backup = stage / 'cta_web.previous'
assert not backup.exists()
os.link(root / 'bin/cta_web', backup)
backup_info = dict(binary_sha256=sha(backup),webroot=str(previous_web),manager_pid=pid)
(stage / 'rollback.json').write_text(json.dumps(backup_info,indent=2)+'\n')
web = root / 'web-releases' / release
assert not web.exists()
web.mkdir()
frontend_root = web / 'manager'
frontend_root.mkdir()
# Keep immutable old assets available to browsers already using the prior page.
if (previous_web / 'manager/assets').is_dir():
    shutil.copytree(previous_web / 'manager/assets', frontend_root / 'assets')
shutil.copytree(stage / 'frontend', frontend_root, dirs_exist_ok=True)
assert (frontend_root / 'index.html').is_file()
shutil.copy2(stage / 'manifest.json', web / 'FRONTEND-MANIFEST.json')
link = root / ('webroot.next.' + release)
link.symlink_to(web)
os.replace(stage / 'cta_web', root / 'bin/cta_web')
os.replace(link, root / 'webroot')
subprocess.run(['systemctl','--user','restart',unit],check=True)
result=dict(release=release,old=backup_info,new_binary_sha256=sha(root/'bin/cta_web'),new_webroot=str((root/'webroot').resolve()))
print(json.dumps(result),flush=True)
