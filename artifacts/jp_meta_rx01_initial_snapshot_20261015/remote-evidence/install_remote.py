"""Install exactly two verified Manager binaries and restart only Manager."""
import hashlib
import json
import os
import pathlib
import shutil
import subprocess
import sys

ROOT = pathlib.Path('/home/ubuntu/crypto_cta_manager')
STAGE = ROOT/'backups/rx01_initial_snapshot_20261015'
manifest=json.loads((STAGE/'release_manifest.json').read_text())
stamp=manifest['release']
assert stamp.isalnum()
assert set(manifest['binaries'])=={'cta_web','nav_rebuild'}
release=ROOT/'web-releases'/stamp/'manager'
release.mkdir(parents=True,exist_ok=False)
for name,wanted in manifest['binaries'].items():
    staged=ROOT/'bin'/f'{name}.next.{stamp}'
    assert hashlib.sha256(staged.read_bytes()).hexdigest()==wanted, name
    assert (ROOT/'bin'/name).is_file()
for name in manifest['binaries']:
    target=ROOT/'bin'/name
    shutil.copy2(target,STAGE/f'{name}.before.{stamp}')
    staged=ROOT/'bin'/f'{name}.next.{stamp}'
    staged.chmod(0o755)
    shutil.copy2(staged,release/name)
    os.replace(staged,target)
shutil.copy2(STAGE/'release_manifest.json',release/'release_manifest.json')
subprocess.run(['systemctl','--user','restart','crypto-cta-manager-web.service'],check=True)
print(json.dumps({'installed':stamp,'restarted':'crypto-cta-manager-web.service','binaries':list(manifest['binaries'])}))
