from pathlib import Path
import datetime,hashlib,json,subprocess
root=Path(__file__).parent
release=datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ')
blob=Path('/mnt/nvme-raid0-28t/fanghaizhou/cargo-target/crypto_cta_manager/release/cta_web')
previous=json.loads((root/'deployment.json').read_text())
diff=subprocess.check_output(['git','diff','--','src/nav.rs','src/nav/funding_estimate.rs','AGENTS.md','docs/funding_and_slippage.md'])
(root/'change.patch').write_bytes(diff)
m={'release':release,'base_commit':subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip(),'patch_sha256':hashlib.sha256(diff).hexdigest(),'binary_sha256':hashlib.sha256(blob.read_bytes()).hexdigest(),'previous_manager_pid':previous['new_manager_pid'],'webroot':previous['webroot'],'scope':'Manager initial-position system-close attribution and negligible virtual exposure; cta_web only'}
(root/'release_manifest_first.json').write_text((root/'release_manifest.json').read_text())
(root/'release_manifest.json').write_text(json.dumps(m,indent=2)+'\n')
subprocess.run(['ssh','-o','BatchMode=yes','cta_exec','mkdir -p /home/el01/crypto_cta_manager/releases/'+release],check=True)
for src,name in [(blob,'cta_web'),(root/'release_manifest.json','manifest.json'),(root/'change.patch','change.patch')]:
 subprocess.run(['scp','-q',str(src),'cta_exec:/home/el01/crypto_cta_manager/releases/'+release+'/'+name],check=True)
print(json.dumps(m))
