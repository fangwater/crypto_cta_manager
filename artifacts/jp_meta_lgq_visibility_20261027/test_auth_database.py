"""Run the grant-scope regression in an isolated, disposable local PostgreSQL."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

base = Path('/mnt/nvme-raid0-28t/fanghaizhou/cta-test-tools/pg16')
bin_dir = base / 'root/usr/lib/postgresql/16/bin'
cluster = Path(tempfile.mkdtemp(prefix='cta-auth-', dir=base))
socket = Path(tempfile.mkdtemp(prefix='cta-auth-test-'))
env = dict(os.environ, LD_LIBRARY_PATH=str(base / 'root/usr/lib/x86_64-linux-gnu'), CTA_AUTH_TEST_SOCKET=str(socket))
started = False
try:
    with (cluster / 'init.log').open('w') as log:
        subprocess.run([str(bin_dir / 'initdb'), '-D', str(cluster / 'data'), '-A', 'trust', '--no-locale'], env=env, stdout=log, stderr=log, check=True)
    subprocess.run([str(bin_dir / 'pg_ctl'), '-D', str(cluster / 'data'), '-l', str(cluster / 'server.log'), '-o', f"-k {socket} -p 15434 -h ''", '-w', 'start'], env=env, check=True)
    started = True
    subprocess.run(['cargo', 'test', '--lib', 'auth::tests::grant_replacement_preserves_hidden_sources_and_caps_regular_users', '--', '--ignored', '--nocapture'], env=env, check=True)
finally:
    if started:
        subprocess.run([str(bin_dir / 'pg_ctl'), '-D', str(cluster / 'data'), '-m', 'fast', '-w', 'stop'], env=env, check=True)
    shutil.rmtree(cluster)
    shutil.rmtree(socket)
    assert not cluster.exists() and not socket.exists()
