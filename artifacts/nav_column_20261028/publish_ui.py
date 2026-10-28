"""Publish the final ECharts presentation without restarting Manager."""
from pathlib import Path
import hashlib,json,subprocess,tarfile
out=Path(__file__).resolve().parent
root=out.parent.parent
m=json.loads((out/'manifest.json').read_text())
m['runtime_commit']=m.pop('commit')
m['frontend_commit']=subprocess.check_output(['git','rev-parse','HEAD'],text=True,cwd=root).strip()
m['release']+='-echarts-'+m['frontend_commit'][:7]
tarpath=root/'.net-value-stage/echarts.tgz'
with tarfile.open(tarpath,'w:gz') as tar:tar.add(root/'frontend/dist',arcname='manager')
m['frontend_sha256']=hashlib.sha256(tarpath.read_bytes()).hexdigest()
remote='/home/ubuntu/crypto_cta_manager/web-releases/'+m['release']
subprocess.run(['ssh','-o','BatchMode=yes','jp-meta-elvpn','install','-d','-m','0755',remote],check=True)
subprocess.run(['scp','-q',str(tarpath),'jp-meta-elvpn:'+remote+'/frontend.tgz'],check=True)
script=(out/'host_common.py').read_text()+'\nM='+repr(m)+'\n'+r'''
import tarfile,shutil
before=protected()
manager_pid=pid
web=ROOT/'web-releases'/M['release']
assert hashlib.sha256((web/'frontend.tgz').read_bytes()).hexdigest()==M['frontend_sha256']
with tarfile.open(web/'frontend.tgz') as tar:tar.extractall(web,filter='data')
shutil.copytree(ROOT/'webroot/manager/assets',web/'manager/assets',dirs_exist_ok=True)
(web/'RELEASE-MANIFEST.json').write_text(json.dumps(M,indent=2)+'\n')
previous=os.readlink(ROOT/'webroot')
link=ROOT/('webroot.next.'+M['release'])
link.symlink_to('web-releases/'+M['release'])
os.replace(link,ROOT/'webroot')
try:
    for path in ['/manager/workspace/','/manager/net-value/','/manager/api/auth/status']:
        with opener.open('http://127.0.0.1:4191'+path,timeout=15) as r:assert r.status==200
    after=protected()
    assert after['processes']==before['processes']
    assert after['config_hashes']==before['config_hashes']
    assert after['binary_sha256']==before['binary_sha256']
    assert manager_pid==subprocess.check_output(['systemctl','--user','show',UNIT,'-p','MainPID','--value'],text=True).strip()
except Exception:
    link.symlink_to(previous)
    os.replace(link,ROOT/'webroot')
    raise
emit('frontend_deployed',M)
'''
r=subprocess.run(['ssh','-o','BatchMode=yes','jp-meta-elvpn','python3','-'],input=script,text=True,capture_output=True)
(out/'frontend_deployment.jsonl').write_text(r.stdout)
print(r.stdout,end='')
if r.returncode:
    print(r.stderr)
    raise SystemExit(r.returncode)
