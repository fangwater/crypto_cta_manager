"""Remove only this verified deployment's staging and temporary backups."""
import json
from pathlib import Path
import subprocess

out = Path(__file__).resolve().parent
manifest = json.loads((out / 'manifest.json').read_text())
assert json.loads((out / 'comparison.json').read_text())['binary_verified']
script = r'''
from pathlib import Path
import hashlib,json,shutil
root=Path('/home/ubuntu/crypto_cta_manager')
for name,value in M['binaries'].items():
    assert hashlib.sha256((root/'bin'/name).read_bytes()).hexdigest()==value
for directory in ['releases','backups']:
    path=root/directory/M['release']
    assert path.parent==root/directory and not path.is_symlink()
    assert path.is_dir()
    shutil.rmtree(path)
    assert not path.exists()
print(json.dumps({'release':M['release'],'staging_removed':True,'temporary_backups_removed':True}))
'''
result = subprocess.run(['ssh', 'jp-meta-elvpn', 'python3 -'], input='M=' + repr(manifest) + '\n' + script,
                        text=True, capture_output=True, check=True)
(out / 'cleanup.json').write_text(result.stdout)
print(result.stdout, end='')
