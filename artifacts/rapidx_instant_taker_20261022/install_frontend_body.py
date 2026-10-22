# Append after remote_common.py. Static files only; preserve the running Manager.
stage = ROOT/'deploy-staging/rapidx_instant_taker_20261022'
manifest = json.loads((stage/'frontend_release.json').read_text())
assert hashlib.sha256((ROOT/'bin/cta_web').read_bytes()).hexdigest() == manifest['binary_sha256']
web = ROOT/'web-releases'/manifest['release']
web.mkdir(exist_ok=False)
subprocess.run(['tar', '-xzf', str(stage/'frontend-auth.tar.gz'), '-C', str(web)], check=True)
for name, digest in manifest['frontend'].items():
    assert hashlib.sha256((web/'manager'/name).read_bytes()).hexdigest() == digest
(web/'RELEASE-MANIFEST.json').write_text(json.dumps(manifest, indent=2)+'\n')
link = ROOT/'webroot.instant-auth-next'
link.symlink_to(web)
os.replace(link, ROOT/'webroot')
assert subprocess.check_output(['systemctl', '--user', 'show', 'crypto-cta-manager-web.service', '-p', 'MainPID', '--value'], text=True).strip() == pid
emit('frontend_installed', manifest)
