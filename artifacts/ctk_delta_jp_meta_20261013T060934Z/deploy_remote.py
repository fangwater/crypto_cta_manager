from pathlib import Path
import hashlib,json,os,shutil,subprocess,time,urllib.request,urllib.error
ROOT=Path('/home/ubuntu/crypto_cta_manager')
STAGE=ROOT/'deploy-staging/ctk-delta-20261013T060934Z'
RELEASE=ROOT/'web-releases/20261013T060934Z'
UNIT='crypto-cta-manager-web.service'
manifest=json.loads((STAGE/'manifest.json').read_text())
for name,digest in manifest['sha256'].items():
    assert hashlib.sha256((STAGE/name).read_bytes()).hexdigest()==digest,name
before=json.loads((STAGE/'before_state.json').read_text())
for name,digest in before['config_sha256'].items():
    assert hashlib.sha256(Path(name).read_bytes()).hexdigest()==digest,name
for pid,value in before['protected_processes'].items():
    assert (Path('/proc')/pid/'stat').read_text().rsplit(')',1)[1].split()[19]==value['start_ticks'],pid
assert not RELEASE.exists()
old_binary=ROOT/'bin/cta_web'
assert hashlib.sha256(old_binary.read_bytes()).hexdigest()=='bf60725a67c04a68cc6758f953c92187c69a1a8034fcde7be9ef09c04d923f26'
shutil.copy2(old_binary,STAGE/'cta_web.previous')
(STAGE/'webroot.previous').write_text(os.readlink(ROOT/'webroot'))
os.chmod(STAGE/'cta_web',old_binary.stat().st_mode)
subprocess.run([str(STAGE/'cta_web'),'--help'],check=True,stdout=subprocess.DEVNULL)
RELEASE.mkdir()
shutil.copytree(STAGE/'frontend',RELEASE/'manager')
shutil.copy2(STAGE/'manifest.json',RELEASE/'RELEASE-MANIFEST.json')
os.replace(STAGE/'cta_web',old_binary)
subprocess.run(['systemctl','--user','restart',UNIT],check=True)
print('Manager restarted; configuration unchanged; waiting for API readiness',flush=True)
opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
ready=False
for attempt in range(120):
    try:
        with opener.open('http://127.0.0.1:4191/manager/api/dashboard',timeout=3) as response:
            ready=response.status==200
    except urllib.error.HTTPError as error:
        ready=error.code==401
    except (OSError,TimeoutError):pass
    if ready:break
    if attempt%6==0:print('Manager cold-start readiness wait',attempt*5,'seconds',flush=True)
    time.sleep(5)
if not ready:
    rollback=ROOT/'bin/cta_web.rollback.20261013T060934Z'
    shutil.copy2(STAGE/'cta_web.previous',rollback);os.replace(rollback,old_binary)
    subprocess.run(['systemctl','--user','restart',UNIT],check=True)
    raise RuntimeError('readiness timeout: restored previous binary; frontend not switched')
link=ROOT/'webroot.next.20261013T060934Z'
link.symlink_to('web-releases/20261013T060934Z');os.replace(link,ROOT/'webroot')
print(json.dumps({'deployed':True,'release':RELEASE.name,'runtime_sha256':hashlib.sha256(old_binary.read_bytes()).hexdigest(),'manager_pid':subprocess.check_output(['systemctl','--user','show',UNIT,'-p','MainPID','--value'],text=True).strip()}),flush=True)
