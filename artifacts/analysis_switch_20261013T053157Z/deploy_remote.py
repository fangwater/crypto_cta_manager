from pathlib import Path
import hashlib,json,os,re,shutil,subprocess,time,tomllib,urllib.request,urllib.error
ROOT=Path('/home/el01/crypto_cta_manager')
STAGE=ROOT/'deploy-staging/analysis-switch-20261013T053157Z'
RELEASE=ROOT/'web-releases/20261013T053157Z'
UNIT='crypto-cta-manager-web.service'
manifest=json.loads((STAGE/'manifest.json').read_text())
for name,digest in manifest['sha256'].items():
    assert hashlib.sha256((STAGE/name).read_bytes()).hexdigest()==digest,name
assert not RELEASE.exists()
config=ROOT/'config/cta-manager.toml'
raw=config.read_text();old=tomllib.loads(raw)
assert old['kline']['enabled'] is True
section=re.search(r'(?ms)^\[kline\]\s*\n(.*?)(?=^\[|\Z)',raw)
assert section
body,n=re.subn(r'(?m)^enabled\s*=\s*true\s*$', '# Operator policy: keep disabled on el01 across restarts and deployments.\nenabled = false',section.group(1),count=1)
assert n==1
new_raw=raw[:section.start(1)]+body+raw[section.end(1):]
expected=tomllib.loads(raw);expected['kline']['enabled']=False
assert tomllib.loads(new_raw)==expected
for source in old['sources']:
    assert Path(source['rocksdb_path']).is_absolute()
old_binary=ROOT/'bin/cta_web'
markers=old_binary.read_bytes()
print(json.dumps({'previous_binary_sha256':hashlib.sha256(markers).hexdigest(),'previous_account_risk_support':{k:k.encode() in markers for k in ['binance_std_um_account_risk','rapidx_account_risk']} }),flush=True)
shutil.copy2(old_binary,STAGE/'cta_web.previous')
shutil.copy2(config,STAGE/'cta-manager.toml.previous')
(STAGE/'webroot.previous').write_text(os.readlink(ROOT/'webroot'))
os.chmod(STAGE/'cta_web',old_binary.stat().st_mode)
subprocess.run([str(STAGE/'cta_web'),'--help'],check=True,stdout=subprocess.DEVNULL)
shutil.copytree(STAGE/'frontend',RELEASE)
shutil.copy2(STAGE/'manifest.json',RELEASE/'RELEASE-MANIFEST.json')
next_config=config.with_name('cta-manager.toml.next.20261013T053157Z')
next_config.write_text(new_raw);os.chmod(next_config,config.stat().st_mode)
os.replace(next_config,config)
os.replace(STAGE/'cta_web',old_binary)
subprocess.run(['systemctl','--user','restart',UNIT],check=True)
print('Manager restarted with kline.enabled=false; waiting for API readiness',flush=True)
opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
ready=False
for attempt in range(120):
    try:
        with opener.open('http://127.0.0.1:10051/manager/api/dashboard',timeout=3) as response:
            ready=response.status==200
    except urllib.error.HTTPError as error:
        ready=error.code==401
    except (OSError,TimeoutError):pass
    if ready:break
    if attempt%6==0:print('Manager cold-start readiness wait',attempt*5,'seconds',flush=True)
    time.sleep(5)
if not ready:
    rollback=ROOT/'bin/cta_web.rollback.20261013T053157Z'
    shutil.copy2(STAGE/'cta_web.previous',rollback);os.replace(rollback,old_binary)
    subprocess.run(['systemctl','--user','restart',UNIT],check=True)
    raise RuntimeError('readiness timeout: restored previous binary, retained kline.enabled=false')
link=ROOT/'webroot.next.20261013T053157Z'
link.symlink_to('web-releases/20261013T053157Z');os.replace(link,ROOT/'webroot')
print(json.dumps({'deployed':True,'release':RELEASE.name,'kline_enabled':tomllib.loads(config.read_text())['kline']['enabled'],'runtime_sha256':hashlib.sha256(old_binary.read_bytes()).hexdigest(),'manager_pid':subprocess.check_output(['systemctl','--user','show',UNIT,'-p','MainPID','--value'],text=True).strip()}),flush=True)
