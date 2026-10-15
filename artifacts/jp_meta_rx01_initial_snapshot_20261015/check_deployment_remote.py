"""Capture/verify Manager-only deployment invariants; credentials stay in memory."""
import hashlib
import json
import os
import pathlib
import subprocess
import sys
import time
import tomllib
import urllib.parse

ROOT = pathlib.Path('/home/ubuntu/crypto_cta_manager')
STAGE = ROOT / 'backups/rx01_initial_snapshot_20261015'
TABLES = ['cta_position_snapshots', 'cta_position_snapshot_entries',
          'cta_strategy_position_snapshots', 'cta_strategy_position_snapshot_entries']
UNIT = pathlib.Path('/home/ubuntu/.config/systemd/user/crypto-cta-manager-web.service')
pid = int(subprocess.check_output(['systemctl','--user','show','crypto-cta-manager-web.service','-p','MainPID','--value'],text=True))
runtime = dict(x.split(b'=',1) for x in pathlib.Path(f'/proc/{pid}/environ').read_bytes().split(b'\0') if b'=' in x)
u = urllib.parse.urlsplit(runtime[b'CRYPTO_CTA_LOCAL_DATABASE_URL'].decode())
env = dict(os.environ, PGHOST=u.hostname, PGPORT=str(u.port or 5432), PGUSER=urllib.parse.unquote(u.username),
           PGPASSWORD=urllib.parse.unquote(u.password or ''), PGDATABASE=u.path.lstrip('/'))

def sql(q):
    r = subprocess.run(['psql','-X','-qAt','-v','ON_ERROR_STOP=1'],input=q,text=True,capture_output=True,env=env)
    assert r.returncode == 0, 'database read failed (details suppressed)'
    return r.stdout.strip()

def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

processes = {}
for proc in pathlib.Path('/proc').iterdir():
    if not proc.name.isdecimal() or int(proc.name) in {pid,os.getpid()}:
        continue
    try:
        comm = (proc/'comm').read_text().strip()
        cmd = (proc/'cmdline').read_bytes()
        if not (any(x in comm.lower() for x in ['trade','exec','viz','nginx','cta_monitor','signal','factor','spread','account_mon','persist'])
                or comm.startswith('python') and any(x in cmd for x in [b'exec_config_server',b'viz_server'])):
            continue
        processes[proc.name] = {'comm':comm,'start_ticks':(proc/'stat').read_text().rsplit(')',1)[1].split()[19]}
    except OSError:
        continue
c = tomllib.loads((ROOT/'config/cta-manager.toml').read_text())
paths = [ROOT/'config/cta-manager.toml',UNIT,
         pathlib.Path('/etc/nginx/snippets/crypto_cta_manager.conf'),
         pathlib.Path('/etc/nginx/sites-available/crypto_proxy_4191.conf'),
         pathlib.Path('/home/ubuntu/nginx_locations.txt')]
paths.extend(pathlib.Path(p) for p in c['kline']['trade_engine_configs'])
state = {'captured_at_us':time.time_ns()//1000,'manager_pid':pid,
         'manager_cpu_affinity':sorted(os.sched_getaffinity(pid)),
         'protected':processes,'config_sha256':{str(p):digest(p) for p in paths if p.is_file()},
         'frontend_sha256':digest(ROOT/'webroot/manager/index.html'),
         'snapshot_hashes':{t:sql(f"SELECT md5(coalesce(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text),'[]')::text) FROM {t} t;") for t in TABLES}}
without_funding=dict(c); without_funding.pop('funding',None)
state['configuration_without_funding_sha256']=hashlib.sha256(json.dumps(without_funding,sort_keys=True).encode()).hexdigest()
mode=sys.argv[1]
assert mode in ['before','after','funding_before','funding_after']
if mode in ['after','funding_after']:
    before=json.loads((STAGE/('deployment_funding_before.json' if mode=='funding_after' else 'deployment_before.json')).read_text())
    keys=['snapshot_hashes','manager_cpu_affinity']
    if mode=='after': keys+=['config_sha256','frontend_sha256']
    else:
        keys+=['configuration_without_funding_sha256']
        for path,wanted in before['config_sha256'].items():
            if path!=str(ROOT/'config/cta-manager.toml'): assert state['config_sha256'][path]==wanted,path
        manifest=json.loads((STAGE/'funding_manifest.json').read_text())
        assert state['frontend_sha256']==manifest['frontend']['index.html']
        assert c['funding']['rapidx_trading_ip_sources']==['binance_exec_trade05','binance_exec_trade06','binance_exec_trade07']
    for k in keys:
        assert state[k]==before[k], k
    for p,identity in before['protected'].items():
        assert state['protected'].get(p)==identity, ('protected process changed',p,identity['comm'])
    assert state['manager_pid']!=before['manager_pid']
(STAGE/f'deployment_{mode}.json').write_text(json.dumps(state,indent=2)+'\n')
print(json.dumps({'mode':mode,'manager_pid':pid,'protected_processes':len(processes),'config_files':len(state['config_sha256']),
                  'snapshot_tables':len(TABLES),'manager_cpu_affinity':state['manager_cpu_affinity']},indent=2))
