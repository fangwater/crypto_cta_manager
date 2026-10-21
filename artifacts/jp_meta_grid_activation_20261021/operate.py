import collections
import copy
import datetime
import hashlib
import json
import os
from pathlib import Path
import secrets
import subprocess
import sys
import time
import urllib.request

ROOT = Path('/home/ubuntu/crypto_cta_manager/backups/grid_activation_20261021')
SOURCES = {'binance_exec_trade05': 'virtual02', 'binance_exec_trade06': 'virtual03', 'binance_exec_trade07': 'virtual04'}
BASE = 'http://127.0.0.1:4191'

def sql(query):
    p = subprocess.run(['sudo', '-n', '-u', 'postgres', 'psql', '-X', '-qAt', '-v', 'ON_ERROR_STOP=1', '-d', 'crypto_cta_manager'], input=query, text=True, capture_output=True)
    if p.returncode:
        raise RuntimeError(p.stderr)
    return p.stdout.strip()

def save(name, value):
    ROOT.mkdir(mode=0o700, parents=True, exist_ok=True)
    path = ROOT / name
    path.write_text(json.dumps(value, indent=2, sort_keys=True))
    path.chmod(0o600)

def get_processes():
    rows = {}
    for p in Path('/proc').iterdir():
        if not p.name.isdigit():
            continue
        try:
            cwd = str((p / 'cwd').resolve())
            if cwd not in ['/home/ubuntu/' + s for s in SOURCES] and cwd != '/home/ubuntu/crypto_cta_manager':
                continue
            exe = (p / 'exe').resolve().name
            if exe in ['bash', 'sh', 'python3.12', 'python3']:
                continue
            rows[p.name] = {'cwd': cwd, 'exe': exe, 'start': (p / 'stat').read_text().split()[21]}
        except (OSError, IndexError):
            pass
    return rows

def main():
    token = secrets.token_urlsafe(32)
    digest = hashlib.sha256(token.encode()).hexdigest()
    session = sql("INSERT INTO cta_auth_sessions(user_id,token_hash,expires_at) SELECT user_id,decode('" + digest + "','hex'),now()+interval '30 minutes' FROM cta_users WHERE role='admin' AND NOT disabled ORDER BY user_id LIMIT 1 RETURNING session_id;")
    assert session.isdigit(), 'No administrator for temporary operation session'
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    def api(path, data=None, method=None):
        req = urllib.request.Request(BASE + path, data=None if data is None else json.dumps(data).encode(), method=method, headers={'Cookie': 'cta_session=' + token, 'Content-Type': 'application/json'})
        with opener.open(req, timeout=45) as r:
            return json.load(r)
    try:
        virtuals = api('/manager/api/catalog/virtual-accounts')
        orders = api('/manager/api/catalog/order-strategies')
        grid = next(o for o in orders if o['strategy_name'] == 'lgq_grid')
        studios = {s: api('/manager/api/catalog/accounts/' + s) for s in SOURCES}
        positions = api('/manager/api/catalog/position-strategies')
        relevant = {b['position_strategy_name'] for a in virtuals if a['virtual_id'] in SOURCES.values() for b in a['bindings']}
        overrides = {p['strategy_name']: p.get('symbol_order_strategy_overrides') for p in positions if p['strategy_name'] in relevant and p.get('symbol_order_strategy_overrides')}
        record = {'time': datetime.datetime.now(datetime.timezone.utc).isoformat(), 'virtuals': virtuals, 'grid': grid, 'studios': studios, 'overrides': overrides, 'processes': get_processes(), 'positions': [p for p in positions if p['strategy_name'] in relevant]}
        mode = sys.argv[1] if len(sys.argv) > 1 else 'inspect'
        stamp = str(time.time_ns())
        save(mode + '_' + stamp + '.json', record)
        print('GRID', json.dumps({'strategy_name': grid['strategy_name'], 'algorithm': grid['order_parameters']['algorithm'], 'grid': grid['order_parameters']['grid']}))
        print('OVERRIDES', json.dumps(overrides))
        for s, v in SOURCES.items():
            a = next(a for a in virtuals if a['virtual_id'] == v)
            print('ACCOUNT', json.dumps({'source': s, 'virtual': v, 'bindings': len(a['bindings']), 'active': sum(b['shares'] > 0 for b in a['bindings']), 'orders': dict(collections.Counter(b['order_strategy_name'] for b in a['bindings'])), 'followers': a['followers'], 'configuration': studios[s]['configuration'], 'pending': studios[s]['pending_publishes']}))
        if mode == 'execute':
            assert not overrides, 'Review per-symbol overrides first'
            assert grid['order_parameters']['algorithm'] == 'grid'
            assert not (ROOT / 'before.json').exists(), 'Existing activation backup; inspect before retry'
            for s, v in SOURCES.items():
                a = next(a for a in virtuals if a['virtual_id'] == v)
                assert {f['source_id'] for f in a['followers']} == {s}, 'Unexpected follower scope'
                assert studios[s]['configuration']['mode'] == 'follow' and studios[s]['configuration']['virtual_id'] == v
                assert not studios[s]['pending_publishes'], 'Pending configuration must finish first'
            save('before.json', record)
            for s, v in SOURCES.items():
                latest = next(a for a in api('/manager/api/catalog/virtual-accounts') if a['virtual_id'] == v)
                old = next(a for a in virtuals if a['virtual_id'] == v)
                assert latest['bindings'] == old['bindings'] and latest['updated_at_us'] == old['updated_at_us'], 'Concurrent configuration edit'
                body = {'name': old['name'], 'bindings': copy.deepcopy(old['bindings'])}
                for b in body['bindings']:
                    b['order_strategy_name'] = 'lgq_grid'
                result = api('/manager/api/catalog/virtual-accounts/' + v, body, 'PUT')
                save('applied_' + v + '.json', result)
                print('APPLIED', s, v, len(result['bindings']), flush=True)
        snapshots = {}
        for s in SOURCES:
            snap = api('/exec_trade' + s[-2:] + '/snapshot')
            snapshots[s] = snap
            st = next(e for e in snap['entries'] if e['type'] == 'exec_pre_trade_state')
            rows = st['entry']['rows']
            print('SNAPSHOT', s, json.dumps({'age_secs': round(time.time()-st['ts_ms']/1000, 3), 'position_ready': st['entry']['position_ready'], 'rows': len(rows), 'algorithms': dict(collections.Counter(r['algorithm'] for r in rows)), 'completion': dict(collections.Counter(r['completion_reason'] for r in rows)), 'incomplete_rows': sum(not r['execution_complete'] for r in rows)}))
        save('snapshots_' + stamp + '.json', snapshots)
        if mode == 'verify':
            before = json.loads((ROOT / 'before.json').read_text())
            verification = {'time': record['time'], 'accounts': {}, 'processes_unchanged': before['processes'] == record['processes'], 'grid_unchanged': before['grid'] == grid}
            for s, v in SOURCES.items():
                old = next(a for a in before['virtuals'] if a['virtual_id'] == v)
                current = next(a for a in virtuals if a['virtual_id'] == v)
                strip = lambda bs: sorted([{k:b[k] for k in ['binding_name','position_strategy_name','shares']} for b in bs], key=lambda b:b['binding_name'])
                assert strip(old['bindings']) == strip(current['bindings'])
                assert before['studios'][s]['configuration'] == studios[s]['configuration']
                assert not studios[s]['pending_publishes']
                assert all(b['order_strategy_name'] == 'lgq_grid' for b in current['bindings'])
                runtime = {}
                mismatches = []
                stamps = []
                for market in ['binance-futures','binance-margin']:
                    for b in current['bindings']:
                        key = s + ':' + market + ':batch_exec:' + b['binding_name']
                        val = json.loads(subprocess.check_output(['redis-cli','--raw','GET',key], text=True))
                        runtime[key] = val
                        assert val['algorithm'] == 'grid' and val['grid'] == grid['order_parameters']['grid']
                        p = next(p for p in positions if p['strategy_name'] == b['position_strategy_name'])
                        stamps.append(p['updated_at_us'])
                        for symbol, target in val['targets'].items():
                            if b['shares'] == 0:
                                if target['qty'] not in [0, 'clear']:
                                    mismatches.append([key, symbol, 'stopped target nonzero'])
                            elif symbol in p['targets']:
                                expected = p['targets'][symbol]
                                if isinstance(expected, dict): expected = expected['qty']
                                if expected != 'clear': expected *= b['shares'] * studios[s]['configuration']['multiplier']
                                actual = target['qty']
                                equal = actual == expected or (isinstance(actual,(float,int)) and isinstance(expected,(float,int)) and abs(actual-expected) <= max(1,abs(expected))*1e-10)
                                if not equal: mismatches.append([key,symbol,actual,expected])
                st = next(e for e in snapshots[s]['entries'] if e['type'] == 'exec_pre_trade_state')
                summary = {'bindings':len(current['bindings']),'active_bindings':sum(b['shares']>0 for b in current['bindings']),'runtime_configs':len(runtime),'pending':studios[s]['pending_publishes'],'target_mismatches':mismatches,'latest_position_publish_us':max(stamps),'state_age_secs':round(time.time()-st['ts_ms']/1000,3)}
                verification['accounts'][s] = summary
                save('runtime_' + s + '_' + stamp + '.json', runtime)
            save('verification.json', verification)
            print('VERIFICATION', json.dumps(verification))
        print('EVIDENCE', str(ROOT), 'at', stamp)
    finally:
        sql('DELETE FROM cta_auth_sessions WHERE session_id=' + session + ';')
        print('TEMP_SESSION_REMOVED')

if __name__ == '__main__':
    main()
