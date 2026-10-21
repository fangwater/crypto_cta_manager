import datetime
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time

ROOT=Path('/home/ubuntu/crypto_cta_manager/backups/grid_split_20261021')
ENV=Path('/home/ubuntu/binance_exec_trade06')
BINARIES=['exec-pre-trade','viz_server']

def save(name,value):
    ROOT.mkdir(parents=True,exist_ok=True,mode=0o700)
    p=ROOT/name
    p.write_text(json.dumps(value,indent=2,sort_keys=True));p.chmod(0o600)

def sha(path):
    h=hashlib.sha256()
    with path.open('rb') as f:
        for block in iter(lambda:f.read(1024*1024),b''):h.update(block)
    return h.hexdigest()

def processes():
    result={}
    for p in Path('/proc').iterdir():
        if not p.name.isdigit():continue
        try:
            cwd=(p/'cwd').resolve()
            exe=(p/'exe').resolve().name
            if not str(cwd).startswith('/home/ubuntu/'):continue
            if exe in ['bash','sh','ssh','sshd','sudo','psql','scp','python3','python3.12','tail']:continue
            stat=(p/'stat').read_text().split()
            result[p.name]={'cwd':str(cwd),'exe':exe,'start':stat[21],'affinity':sorted(os.sched_getaffinity(int(p.name)))}
        except (OSError,ValueError,IndexError):pass
    return result

def config_hashes():
    paths=set()
    for env in Path('/home/ubuntu').iterdir():
        if not env.is_dir() or not (env/'env.sh').exists():continue
        for name in ['env.sh','trade_engine.toml']:
            p=env/name
            if p.is_file():paths.add(p)
        paths.update(p for p in (env/'config').glob('*.toml') if p.is_file())
    paths.add(Path('/home/ubuntu/crypto_cta_manager/config/cta-manager.toml'))
    return {str(p):sha(p) for p in sorted(paths)}

def snapshot():
    return {'at':datetime.datetime.now(datetime.timezone.utc).isoformat(),'processes':processes(),'configs':config_hashes(),'binaries':{n:sha(ENV/n) for n in BINARIES}}

def run_script(name):
    print('RUN_SCRIPT',name,flush=True)
    log=ROOT/(name+'.log')
    with log.open('ab') as out:
        result=subprocess.run(['bash',str(ENV/'scripts'/name)],cwd=ENV,stdout=out,stderr=subprocess.STDOUT,timeout=120)
    if result.returncode:raise RuntimeError(name+' failed; inspect host-local log')
    print('SCRIPT_OK',name,flush=True)

def main():
    mode=sys.argv[1]
    if mode=='prepare':
        assert not (ROOT/'deployment_before.json').exists()
        before=snapshot()
        for n in BINARIES:shutil.copy2(ENV/n,ROOT/(n+'.before'))
        save('deployment_before.json',before)
        print('PREPARED',len(before['processes']),len(before['configs']),before['binaries'])
    elif mode=='install':
        before=json.loads((ROOT/'deployment_before.json').read_text())
        manifest=json.loads((ROOT/'build_manifest.json').read_text())
        assert config_hashes()==before['configs'],'Configuration changed since backup'
        for n in BINARIES:
            assert sha(ENV/n)==before['binaries'][n],'Runtime binary changed concurrently'
            assert sha(ROOT/(n+'.next'))==manifest['binaries'][n]
            assert sha(ROOT/(n+'.before'))==before['binaries'][n]
        run_script('stop_exec_viz_server.sh')
        run_script('stop_exec_pre_trade.sh')
        alive=processes()
        assert not any(p['cwd']==str(ENV) and p['exe'] in BINARIES for p in alive.values())
        for n in BINARIES:
            staged=ENV/(n+'.grid_state_next')
            assert not staged.exists()
            shutil.copy2(ROOT/(n+'.next'),staged);staged.chmod(0o755)
            assert sha(staged)==manifest['binaries'][n]
            os.replace(staged,ENV/n)
        save('installed.json',{'at':datetime.datetime.now(datetime.timezone.utc).isoformat(),'binaries':manifest['binaries']})
        run_script('start_exec_pre_trade.sh')
        run_script('start_exec_viz_server.sh')
        after=snapshot();save('deployment_after.json',after)
        changed={pid:p for pid,p in before['processes'].items() if after['processes'].get(pid)!=p}
        protected_changed={pid:p for pid,p in changed.items() if not(p['cwd']==str(ENV) and p['exe'] in BINARIES)}
        print('DEPLOYMENT',json.dumps({'changed':changed,'protected_changed':protected_changed,'configs_unchanged':before['configs']==after['configs'],'new_rx01':[dict(pid=pid,**p) for pid,p in after['processes'].items() if p['cwd']==str(ENV) and p['exe'] in BINARIES]}),flush=True)
        assert not protected_changed
        assert before['configs']==after['configs']
    elif mode=='verify':
        before=json.loads((ROOT/'deployment_before.json').read_text())
        after=snapshot();save('deployment_final.json',after)
        changed={pid:p for pid,p in before['processes'].items() if after['processes'].get(pid)!=p and not(p['cwd']==str(ENV) and p['exe'] in BINARIES)}
        print('FINAL',json.dumps({'protected_changed':changed,'configs_unchanged':before['configs']==after['configs'],'rx01':[dict(pid=pid,**p) for pid,p in after['processes'].items() if p['cwd']==str(ENV) and p['exe'] in BINARIES]}))
        assert not changed and before['configs']==after['configs']
    else:raise ValueError(mode)

if __name__=='__main__':main()
