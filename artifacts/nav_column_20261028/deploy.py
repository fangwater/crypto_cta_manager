"""Deploy only jp-meta CTA Manager API/UI, with exact rollback targets."""
from pathlib import Path
import hashlib,json,subprocess,tarfile,datetime
out=Path(__file__).resolve().parent
root=out.parent.parent
release='net-value-'+datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ')
stage=root/'.net-value-stage'
stage.mkdir(mode=0o700,exist_ok=True)
assert not (stage/'frontend.tgz').exists()
with tarfile.open(stage/'frontend.tgz','w:gz') as tar:
    tar.add(root/'frontend/dist',arcname='manager')
binary=Path('/mnt/nvme-raid0-28t/fanghaizhou/cargo-target/crypto_cta_manager/release/cta_web')
sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
manifest={'release':release,'commit':subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip(),'binary_sha256':sha(binary),'frontend_sha256':sha(stage/'frontend.tgz')}
(out/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
host='jp-meta-elvpn'
remote_root='/home/ubuntu/crypto_cta_manager'
remote_stage=remote_root+'/releases/'+release
subprocess.run(['ssh','-o','BatchMode=yes',host,'install','-d','-m','0700',remote_stage],check=True)
subprocess.run(['scp','-q',str(binary),str(stage/'frontend.tgz'),host+':'+remote_stage+'/'],check=True)
common=(out/'host_common.py').read_text()
script=r'''
import shutil,tarfile
assert subprocess.check_output(['hostname','-f'],text=True).strip()=='ip-172-31-35-228.ap-northeast-1.compute.internal'
assert subprocess.check_output(['whoami'],text=True).strip()=='ubuntu'
assert subprocess.check_output(['systemctl','--user','is-active',UNIT],text=True).strip()=='active'
assert (ROOT/'bin/cta_web').is_file() and (ROOT/'webroot/manager/index.html').is_file()
protected_before=protected()
nav_pid=subprocess.check_output(['systemctl','show','crypto-nav-manager.service','-p','MainPID','--value'],text=True).strip()
stage=ROOT/'releases'/M['release']
sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
assert sha(stage/'cta_web')==M['binary_sha256']
assert sha(stage/'frontend.tgz')==M['frontend_sha256']
backup=ROOT/'backups'/M['release']
backup.mkdir(parents=True,mode=0o700)
os.link(ROOT/'bin/cta_web',backup/'cta_web.previous')
previous=os.readlink(ROOT/'webroot')
(backup/'rollback.json').write_text(json.dumps({'webroot':previous,'binary':protected_before['binary_sha256'],'protected':protected_before})+'\n')
web=ROOT/'web-releases'/M['release']
web.mkdir(mode=0o755)
with tarfile.open(stage/'frontend.tgz') as tar:tar.extractall(web,filter='data')
shutil.copytree(ROOT/'webroot/manager/assets',web/'manager/assets',dirs_exist_ok=True)
(web/'RELEASE-MANIFEST.json').write_text(json.dumps(M,indent=2)+'\n')
def link_to(target):
    next_link=ROOT/('webroot.next.'+M['release'])
    next_link.symlink_to(target)
    os.replace(next_link,ROOT/'webroot')
try:
    os.chmod(stage/'cta_web',0o755)
    os.replace(stage/'cta_web',ROOT/'bin/cta_web')
    link_to('web-releases/'+M['release'])
    subprocess.run(['systemctl','--user','restart',UNIT],check=True)
    ready=False
    for _ in range(600):
        try:
            with opener.open('http://127.0.0.1:4191/manager/api/auth/status',timeout=2) as r:ready=r.status==200
            if ready:break
        except Exception:pass
        time.sleep(1)
    assert ready,'Manager gateway did not become ready'
    with opener.open('http://127.0.0.1:4191/manager/net-value/',timeout=10) as r:assert r.status==200
    after=protected()
    assert after['processes']==protected_before['processes'],'protected process changed'
    assert after['config_hashes']==protected_before['config_hashes'],'configuration changed'
    assert after['tables']==protected_before['tables'],'protected catalog/snapshot changed'
    assert after['manager_affinity']==protected_before['manager_affinity'],'Manager affinity changed'
    assert nav_pid==subprocess.check_output(['systemctl','show','crypto-nav-manager.service','-p','MainPID','--value'],text=True).strip(),'NAV process changed'
    assert after['binary_sha256']==M['binary_sha256']
    emit('deployed',{'release':M['release'],'commit':M['commit'],'protected_processes':len(after['processes']),'config_hashes':len(after['config_hashes']),'manager_affinity':after['manager_affinity'],'snapshots_unchanged':True,'nav_unchanged':True})
except Exception:
    replacement=stage/'cta_web.rollback'
    os.link(backup/'cta_web.previous',replacement)
    os.replace(replacement,ROOT/'bin/cta_web')
    link_to(previous)
    subprocess.run(['systemctl','--user','restart',UNIT],check=True)
    emit('rolled_back',{'release':M['release']})
    raise
'''
r=subprocess.run(['ssh','-o','BatchMode=yes',host,'python3','-'],input=common+'\nM='+repr(manifest)+'\n'+script,text=True,capture_output=True)
(out/'deployment.jsonl').write_text(r.stdout)
print(r.stdout,end='')
if r.returncode:
    print(r.stderr)
    raise SystemExit(r.returncode)
