import hashlib, json, os, re, shutil, subprocess, sys
from pathlib import Path
root=Path('/home/el01/crypto_cta_manager')
release=sys.argv[1]
assert re.fullmatch(r'\d{8}T\d{6}Z',release)
stage=root/'releases'/release
manifest=json.loads((stage/'manifest.json').read_text())
sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
assert manifest['release']==release
assert sha(stage/'cta_web')==manifest['binary_sha256']
unit='crypto-cta-manager-web.service'
pid=subprocess.check_output(['systemctl','--user','show',unit,'-p','MainPID','--value'],text=True).strip()
assert int(pid)==manifest['previous_manager_pid'],'Manager changed since baseline; review before install'
assert str(Path('/proc',pid,'exe').resolve())==str(root/'bin/cta_web')
assert str((root/'webroot').resolve())==manifest['webroot']
backup=stage/'cta_web.previous'
assert not backup.exists()
os.link(root/'bin/cta_web',backup)
old={'manager_pid':int(pid),'binary_sha256':sha(backup),'webroot':str((root/'webroot').resolve())}
(stage/'rollback.json').write_text(json.dumps(old,indent=2)+'\n')
os.replace(stage/'cta_web',root/'bin/cta_web')
try:
 subprocess.run(['systemctl','--user','restart',unit],check=True)
 subprocess.run(['systemctl','--user','is-active','--quiet',unit],check=True)
except Exception:
 rollback=stage/'cta_web.rollback'
 shutil.copy2(backup,rollback)
 os.replace(rollback,root/'bin/cta_web')
 subprocess.run(['systemctl','--user','restart',unit],check=True)
 raise
new_pid=subprocess.check_output(['systemctl','--user','show',unit,'-p','MainPID','--value'],text=True).strip()
print(json.dumps({'release':release,'old':old,'new_manager_pid':int(new_pid),'binary_sha256':sha(root/'bin/cta_web'),'webroot':str((root/'webroot').resolve()),'backup':str(backup)}))
