"""Atomically replace Manager binaries and frontend after explicit schema setup."""
import hashlib
import json
import os
import pathlib
import shutil
import subprocess
import tarfile

ROOT=pathlib.Path('/home/ubuntu/crypto_cta_manager')
STAGE=ROOT/'backups/rx01_initial_snapshot_20261015'
m=json.loads((STAGE/'funding_manifest.json').read_text()); stamp=m['release']
assert stamp.isalnum() and set(m['binaries'])=={'cta_web','nav_rebuild'}
release=ROOT/'web-releases'/stamp
assert not release.exists(); (release/'manager').mkdir(parents=True); (release/'binaries').mkdir()
for name,wanted in m['binaries'].items():
    staged=ROOT/'bin'/f'{name}.next.{stamp}'
    assert hashlib.sha256(staged.read_bytes()).hexdigest()==wanted
archive=STAGE/'funding_frontend.tar.gz'
assert hashlib.sha256(archive.read_bytes()).hexdigest()==m['frontend_archive_sha256']
with tarfile.open(archive,'r:gz') as t: t.extractall(release/'manager',filter='data')
assert {p.relative_to(release/'manager').as_posix() for p in (release/'manager').rglob('*') if p.is_file()}==set(m['frontend'])
for name,wanted in m['frontend'].items():
    assert hashlib.sha256((release/'manager'/name).read_bytes()).hexdigest()==wanted
old_webroot=os.readlink(ROOT/'webroot')
(STAGE/'webroot_before_funding.txt').write_text(old_webroot+'\n')
for name in m['binaries']:
    shutil.copy2(ROOT/'bin'/name,STAGE/f'{name}.before-funding.{stamp}')
    staged=ROOT/'bin'/f'{name}.next.{stamp}'; staged.chmod(0o755)
    shutil.copy2(staged,release/'binaries'/name)
    os.replace(staged,ROOT/'bin'/name)
shutil.copy2(STAGE/'funding_manifest.json',release/'RELEASE-MANIFEST.json')
subprocess.run(['systemctl','--user','restart','crypto-cta-manager-web.service'],check=True)
next_link=ROOT/f'webroot.next.{stamp}'
next_link.symlink_to('web-releases/'+stamp,target_is_directory=True)
os.replace(next_link,ROOT/'webroot')
print(json.dumps({'installed':stamp,'manager_only_restart':True,'frontend_updated':True}))
