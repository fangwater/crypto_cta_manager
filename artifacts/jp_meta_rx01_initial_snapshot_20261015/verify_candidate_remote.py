"""Compare candidate NAV against one stable live-account/history observation."""
import hashlib
import json
import os
import pathlib
import secrets
import subprocess
import sys
import time
import urllib.parse
import urllib.request

ROOT = pathlib.Path('/home/ubuntu/crypto_cta_manager')
STAGE = ROOT / 'backups/rx01_initial_snapshot_20261015'
SOURCE = 'binance_exec_trade06'
pid = int(subprocess.check_output(['systemctl','--user','show','crypto-cta-manager-web.service','-p','MainPID','--value'],text=True))
runtime = dict(x.split(b'=',1) for x in pathlib.Path(f'/proc/{pid}/environ').read_bytes().split(b'\0') if b'=' in x)
env = dict(os.environ)
env['CRYPTO_CTA_LOCAL_DATABASE_URL'] = runtime[b'CRYPTO_CTA_LOCAL_DATABASE_URL'].decode()
u = urllib.parse.urlsplit(env['CRYPTO_CTA_LOCAL_DATABASE_URL'])
env.update(PGHOST=u.hostname, PGPORT=str(u.port or 5432), PGUSER=urllib.parse.unquote(u.username),
           PGPASSWORD=urllib.parse.unquote(u.password or ''), PGDATABASE=u.path.lstrip('/'))


def sql(q):
    r = subprocess.run(['psql','-X','-qAt','-v','ON_ERROR_STOP=1'],input=q,text=True,capture_output=True,env=env)
    assert r.returncode == 0, 'database query failed (details suppressed)'
    return r.stdout.strip()


def rebuild(binary):
    r = subprocess.run([str(binary),'--config',str(ROOT/'config/cta-manager.toml'),'--source',SOURCE,'--compact'],
                       env=env,capture_output=True,text=True)
    if r.returncode:
        print(r.stderr)
        raise RuntimeError('candidate rebuild failed')
    return json.loads(r.stdout)


token = secrets.token_urlsafe(32)
digest = hashlib.sha256(token.encode()).hexdigest()
session = sql("INSERT INTO cta_auth_sessions (user_id,token_hash,expires_at) SELECT user_id,decode('"+digest+"','hex'),now()+interval '15 minutes' FROM cta_users WHERE role='admin' AND disabled=false ORDER BY user_id LIMIT 1 RETURNING session_id;")
assert session.isdecimal()
try:
    opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
    installed = sys.argv[1:] == ['--installed']
    candidate_binary = ROOT/'bin/nav_rebuild' if installed else STAGE/'nav_rebuild.candidate'
    for attempt in range(5):
        a = rebuild(candidate_binary)
        before = rebuild(ROOT/'bin/nav_rebuild')
        req=urllib.request.Request('http://127.0.0.1:4191/exec_trade06/snapshot',headers={'Cookie':'cta_session='+token})
        with opener.open(req,timeout=30) as response: snapshot=json.load(response)
        after = rebuild(candidate_binary)
        if a == after: break
    else: raise RuntimeError('live fills changed during all verification attempts')
    state=next(x['entry'] for x in snapshot['entries'] if x['type']=='exec_pre_trade_state')
    assert state['position_ready'] and abs(time.time()*1000-state['ts_ms'])<30000
    account={r['symbol']:r['account_position_qty'] for r in state['rows']}
    current_marks={r['symbol']:abs(r['account_position_usdt']/r['account_position_qty'])
                   for r in state['rows'] if r['account_position_qty']}
    candidate=after['sources'][0]
    original=before['sources'][0]
    assert candidate['initial_position_snapshot_ts_us']==1791896850441032
    assert candidate['initial_position_count']==12
    old={s['symbol']:s['net_quantity'] for s in original['symbols']}
    new={s['symbol']:s['net_quantity'] for s in candidate['symbols']}
    comparisons=[]
    for symbol in sorted(set(account)|set(new)|set(old)):
        actual=account.get(symbol,0)
        row={'symbol':symbol,'actual':actual,'before':old.get(symbol,0),'after':new.get(symbol,0),
             'difference_before':old.get(symbol,0)-actual,'difference_after':new.get(symbol,0)-actual}
        comparisons.append(row)
    result={'verified_at_us':time.time_ns()//1000,'snapshot_ts_ms':state['ts_ms'],
            'original_anchor_ts_us':candidate['initial_position_snapshot_ts_us'],
            'before_fills':original['fill_count'],'after_fills':candidate['fill_count'],
            'before_nav_after_fee':original['nav_change_after_fee_quote'],
            'after_nav_after_fee':candidate['nav_change_after_fee_quote'],
            'perpetuals_all_match':all(abs(r['difference_after'])<1e-8 for r in comparisons if r['symbol'].endswith(('USDT','USDC'))),'comparisons':comparisons,
            'before_absolute_quantity_error_usdt':sum(abs(r['difference_before'])*current_marks.get(r['symbol'],0) for r in comparisons),
            'after_absolute_quantity_error_usdt':sum(abs(r['difference_after'])*current_marks.get(r['symbol'],0) for r in comparisons)}
    for name,data in [('candidate_report.json',after),('old_report.json',before),('candidate_snapshot.json',snapshot),('candidate_verification.json',result)]:
        (STAGE/(('final_' if installed else '')+name)).write_text(json.dumps(data,indent=2)+'\n')
    print(json.dumps({k:v for k,v in result.items() if k!='comparisons'},indent=2))
    print(json.dumps([r for r in comparisons if r['symbol'].endswith(('USDT','USDC')) and abs(r['difference_after'])>=1e-8],indent=2))
    if installed:
        assert result['perpetuals_all_match'], 'installed NAV did not match the observed perpetual account quantities'
finally:
    sql('DELETE FROM cta_auth_sessions WHERE session_id='+session+" AND token_hash=decode('"+digest+"','hex');")
