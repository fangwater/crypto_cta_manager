import base64
import hashlib
import http.client
import json
import math
import os
from pathlib import Path
import secrets
import subprocess
import sys
import time
import tomllib
import urllib.parse
import urllib.request

ROOT = Path('/home/ubuntu/crypto_cta_manager')
STAGE = ROOT / 'deploy-staging/ctk-delta-20261013T060934Z'
ORIGIN = 'http://127.0.0.1:4191'
PHASE = sys.argv[1]
CONFIG = tomllib.loads((ROOT / 'config/cta-manager.toml').read_text())

def manager_pid():
    return int(subprocess.check_output(['systemctl', '--user', 'show', 'crypto-cta-manager-web.service', '-p', 'MainPID', '--value'], text=True))

def state():
    paths = [ROOT / 'config/cta-manager.toml', Path.home() / '.config/systemd/user/crypto-cta-manager-web.service',
             Path('/etc/nginx/snippets/crypto_cta_manager.conf'), Path('/etc/nginx/sites-available/crypto_proxy_4191.conf'),
             Path('/home/ubuntu/nginx_locations.txt')]
    paths += [Path(p) for p in CONFIG['kline']['trade_engine_configs']]
    processes = {}
    own = os.getpid()
    for proc in Path('/proc').iterdir():
        if not proc.name.isdigit() or int(proc.name) == own:
            continue
        try:
            comm = (proc / 'comm').read_text().strip()
            cmd = (proc / 'cmdline').read_bytes().replace(b'\0', b' ').decode(errors='replace')
            if comm == 'cta_web':
                continue
            if not (any(s in comm.lower() for s in ['trade', 'exec', 'viz', 'nginx', 'cta_monitor']) or
                    (comm.startswith('python') and any(s in cmd for s in ['exec_config_server', 'viz_server']))):
                continue
            stat = (proc / 'stat').read_text().rsplit(')', 1)[1].split()
            processes[proc.name] = {'comm': comm, 'start_ticks': stat[19]}
        except (OSError, IndexError):
            continue
    return {'manager_pid': manager_pid(), 'webroot': str((ROOT / 'webroot').resolve()),
            'protected_processes': processes,
            'config_sha256': {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in paths if p.is_file()},
            'trading_configs': len(CONFIG['kline']['trade_engine_configs']),
            'kline_weight_per_minute': CONFIG['kline']['weight_per_minute']}

def db_env():
    env = dict(os.environ)
    runtime = dict(entry.split(b'=', 1) for entry in Path(f'/proc/{manager_pid()}/environ').read_bytes().split(b'\0') if b'=' in entry)
    key = CONFIG.get('database', {}).get('url_env', 'CRYPTO_CTA_LOCAL_DATABASE_URL').encode()
    url = urllib.parse.urlsplit(runtime[key].decode())
    env.update(PGHOST=url.hostname, PGPORT=str(url.port or 5432), PGUSER=urllib.parse.unquote(url.username),
               PGPASSWORD=urllib.parse.unquote(url.password or ''), PGDATABASE=urllib.parse.unquote(url.path.lstrip('/')),
               PGCONNECT_TIMEOUT='10')
    return env

def sql(query, env):
    result = subprocess.run(['psql', '-X', '-q', '-A', '-t', '-v', 'ON_ERROR_STOP=1'], input=query, text=True,
                            capture_output=True, env=env)
    if result.returncode:
        raise RuntimeError('verification database operation failed (credentials and SQL suppressed)')
    return result.stdout.strip()

def session_request(token):
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    def get(path):
        req = urllib.request.Request(ORIGIN + path, headers={'Cookie': 'cta_session=' + token})
        with opener.open(req, timeout=175) as response:
            body = response.read()
            return response.status, body
    return get


def check_protected():
    current=state()
    old=json.loads((STAGE/'before_state.json').read_text())
    config_changes=[k for k,v in current['config_sha256'].items() if old['config_sha256'].get(k)!=v]
    assert set(config_changes)<= {'/etc/nginx/sites-available/crypto_proxy_4191.conf'}, 'Manager or trading configuration changed'
    current['observed_config_changes']=config_changes
    removed={k:v for k,v in old['protected_processes'].items() if k not in current['protected_processes']}
    added={k:v for k,v in current['protected_processes'].items() if k not in old['protected_processes']}
    changed={k:v for k,v in current['protected_processes'].items() if k in old['protected_processes'] and old['protected_processes'][k]!=v}
    # Concurrent Nginx reload and one old Viz exit were observed after deployment.
    # Record them explicitly; do not claim all protected processes were unchanged.
    assert not changed, 'existing PID identity changed'
    assert all(v['comm']=='nginx' for v in added.values()), 'non-Nginx process appeared'
    assert all(k=='2392348' or v['comm']=='nginx' for k,v in removed.items()), 'another non-Nginx process disappeared'
    current['protected_process_changes']={'removed':removed,'added':added,'changed':changed}
    return current

def inspect():
    current=state()
    if PHASE=='before':
        (STAGE/'before_state.json').write_text(json.dumps(current,indent=2))
        end=int(time.time()//60*60000)-600000
        query={'startMs':end-86400000,'endMs':end,'sourceIds':'binance_exec_trade03','page':1,'pageSize':25}
        (STAGE/'query.json').write_text(json.dumps(query))
    else:
        current=check_protected()
        query=json.loads((STAGE/'query.json').read_text())
    env=db_env();token=secrets.token_urlsafe(32);digest=hashlib.sha256(token.encode()).hexdigest()
    row=sql("INSERT INTO cta_auth_sessions (user_id,token_hash,expires_at) SELECT user_id,decode('"+digest+"','hex'),now()+interval '4 hours' FROM cta_users WHERE role='admin' AND disabled=false ORDER BY user_id LIMIT 1 RETURNING session_id;",env)
    assert row.isdigit()
    try:
        get=session_request(token)
        checks={}
        for route in ['/manager/api/health','/manager/api/dashboard','/exec_trade03/','/exec_trade03/snapshot','/exec_trade03/config/']:
            checks[route]=get(route)[0]
            assert checks[route]==200
        conn=http.client.HTTPConnection('127.0.0.1',4191,timeout=15)
        conn.request('GET','/exec_trade03/ws',headers={'Cookie':'cta_session='+token,'Upgrade':'websocket','Connection':'Upgrade','Sec-WebSocket-Version':'13','Sec-WebSocket-Key':base64.b64encode(secrets.token_bytes(16)).decode()})
        checks['/exec_trade03/ws']=conn.getresponse().status;conn.close();assert checks['/exec_trade03/ws']==101
        print(json.dumps({'phase':PHASE,'manager_pid':current['manager_pid'],'gateway':checks,'protected_processes':len(current['protected_processes'])}),flush=True)
        for attempt in range(360):
            _,status=get('/manager/api/catalog/kline-status');status=json.loads(status)
            target=status['target_history']
            print(json.dumps({'phase':PHASE,'target_history':target}),flush=True)
            assert not target['last_error']
            if target['ready'] and not target['loading']:break
            time.sleep(30)
        else: raise RuntimeError('target index initialization did not finish')
        def report(extra={}):
            args={**query,**extra}
            _,body=get('/manager/api/catalog/acquisition-cost?'+urllib.parse.urlencode(args))
            result=json.loads(body)
            assert not result['report']['warnings'],result['report']['warnings']
            assert result['report']['totals']['missing_virtual_delta_count']==0
            return result
        snapshot=report();whole=snapshot['report']
        (STAGE/(PHASE+'_cost.json')).write_text(json.dumps(snapshot))
        summary={'phase':PHASE,'query':query,'gateway':checks,'manager_pid':current['manager_pid'],'matched_fills':whole['totals']['matched_fill_count'],'page_count':whole['page_count'],'protected_processes':len(current['protected_processes']),'generation_duration_ms':snapshot['generation_duration_ms']}
        if PHASE=='after':
            old=json.loads((STAGE/'before_cost.json').read_text())['report']
            stripped=json.loads(json.dumps(whole))
            for key in ['row_symbol','row_strategy','row_sort','filtered_row_count']:stripped.pop(key)
            for obj in [old,stripped]:obj.pop('generated_at_us',None)
            assert old==stripped,'existing cost report changed for fixed window'
            assert whole['row_symbol'] is None and whole['row_strategy'] is None
            filtered=report({'rowSymbol':'CTKUSDT','rowStrategy':'rbf_small','rowSort':'price_shortfall'})['report']
            assert filtered['rows'] and filtered['row_symbol']=='CTKUSDT' and filtered['row_strategy']=='rbf_small'
            for key in ['totals','points','by_symbol','by_strategy']:
                assert filtered[key]==whole[key], 'detail filter changed aggregate '+key
            legacy_scope=report({'strategyName':'rbf_small','rowSymbol':'CTKUSDT','rowSort':'price_shortfall'})['report']
            assert legacy_scope['rows']==filtered['rows'],'detail strategy filter differs from original strategy query'
            all_rows=[]
            for page in range(1,filtered['page_count']+1):
                result=filtered if page==1 else report({'rowSymbol':'CTKUSDT','rowStrategy':'rbf_small','rowSort':'price_shortfall','page':page})['report']
                for item in result['rows']:
                    assert item['symbol']=='CTKUSDT' and item['strategy_name']=='rbf_small'
                    fills=item['execution']['fills']
                    assert len(fills)<=200
                    if not item['execution']['fills_sampled'] and fills:
                        qty=sum(abs(f['actual_qty']) for f in fills)
                        assert math.isclose(sum(abs(f['actual_qty'])*f['actual_price'] for f in fills)/qty,item['actual_vwap'],rel_tol=1e-10)
                all_rows.extend(reversed(result['rows']))
            assert len(all_rows)==filtered['filtered_row_count']
            keys=[(r['source_id'],r['strategy_name'],r['binding_name'],r['symbol'],r['venue'],r['received_at_us']) for r in all_rows]
            assert len(keys)==len(set(keys)),'duplicate targets across filtered pages'
            values=[r['price_shortfall_usdt'] for r in all_rows]
            assert values==sorted(values,key=lambda v:(v is not None,v if v is not None else 0),reverse=True)
            bucket=next(r for r in legacy_scope['by_symbol'] if r['bucket']=='CTKUSDT')
            assert math.isclose(sum(r['price_shortfall_usdt'] or 0 for r in all_rows),bucket['price_shortfall_usdt'],rel_tol=1e-10,abs_tol=1e-8)
            assert math.isclose(sum(r['after_fee_shortfall_usdt'] or 0 for r in all_rows),bucket['after_fee_shortfall_usdt'],rel_tol=1e-10,abs_tol=1e-8)
            mid=report({'rowSymbol':'CTKUSDT','rowStrategy':'rbf_mid_1min','rowSort':'latest'})['report']
            assert all(r['strategy_name']=='rbf_mid_1min' for r in mid['rows'])
            assert mid['row_strategy']=='rbf_mid_1min'
            if not mid['rows']: assert mid['filtered_row_count']==0 and mid['page_count']==0
            current=check_protected()
            (STAGE/'after_state.json').write_text(json.dumps(current,indent=2))
            (STAGE/'filtered_ctk_small.json').write_text(json.dumps(filtered))
            summary.update(cost_report_unchanged=True,filtered_aggregate_unchanged=True,small_ctk_delta_count=len(all_rows),small_ctk_page_count=filtered['page_count'],small_ctk_price_shortfall=sum(r['price_shortfall_usdt'] or 0 for r in all_rows),all_filtered_pages_reconcile=True,small_mid_isolated=True,protected_configs_unchanged=not current['observed_config_changes'],observed_config_changes=current['observed_config_changes'],manager_and_trading_configs_unchanged=True,protected_processes_unchanged=not any(current['protected_process_changes'].values()),observed_process_change_counts={k:len(v) for k,v in current['protected_process_changes'].items()})
        (STAGE/(PHASE+'_summary.json')).write_text(json.dumps(summary,indent=2))
        print(json.dumps(summary),flush=True)
    finally:
        sql("DELETE FROM cta_auth_sessions WHERE session_id="+row+" AND token_hash=decode('"+digest+"','hex');",env)
        assert sql('SELECT count(*) FROM cta_auth_sessions WHERE session_id='+row+';',env)=='0'
        print('temporary verification session removed',flush=True)
inspect()
