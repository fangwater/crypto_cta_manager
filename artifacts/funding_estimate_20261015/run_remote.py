"""Run the read-only funding check using the running Manager's private DB environment."""
import hashlib
import json
import os
import pathlib
import subprocess
import sys

root=pathlib.Path.home()/'crypto_cta_manager'
stage=root/'backups/funding_estimate_20261015'
pid=int(subprocess.check_output(['systemctl','--user','show','crypto-cta-manager-web.service','-p','MainPID','--value'],text=True))
runtime=dict(x.split(b'=',1) for x in pathlib.Path(f'/proc/{pid}/environ').read_bytes().split(b'\0') if b'=' in x)
env=os.environ.copy()
for name in [b'CRYPTO_CTA_LOCAL_DATABASE_URL']:
    if name in runtime: env[name.decode()]=runtime[name].decode()
config=root/'config/cta-manager.toml'
before=hashlib.sha256(config.read_bytes()).hexdigest()
args=[str(stage/'nav_funding_check'),'--config',str(config),'--output',str(stage/'comparison.json')]
for source in sys.argv[1:]: args.extend(['--source',source])
# Public rate reads may use any IP per the operator; retain the existing
# dedicated public-data binding on el01 for predictable network reachability.
if pathlib.Path.home().name=='el01': args.extend(['--local-ip','154.197.32.9'])
result=subprocess.run(['nice','-n','10',*args],env=env,text=True,capture_output=True)
assert hashlib.sha256(config.read_bytes()).hexdigest()==before
assert int(subprocess.check_output(['systemctl','--user','show','crypto-cta-manager-web.service','-p','MainPID','--value'],text=True))==pid
if result.returncode:
    # The checker emits public endpoint errors and identifiers only. Filter any
    # credential-shaped text even on a database connection failure.
    error=result.stderr
    for name,value in env.items():
        if ('DATABASE_URL' in name or 'SECRET' in name or 'TOKEN' in name) and value: error=error.replace(value,'[REDACTED]')
    print(error[-3000:]); sys.exit(result.returncode)
print(result.stderr)
print(json.dumps({'config_unchanged':True,'manager_pid_unchanged':True,'output':str(stage/'comparison.json')}))
