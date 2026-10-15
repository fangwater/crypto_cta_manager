import base64
import datetime
import hashlib
import http.client
import json
import math
import os
from pathlib import Path
import re
import subprocess
import time
import tomllib
import urllib.parse
root=Path('/home/el01/crypto_cta_manager')
config=tomllib.loads((root/'config/cta-manager.toml').read_text())
def runtime_snapshot():
    processes = []
    for proc in Path('/proc').iterdir():
        if not proc.name.isdigit():
            continue
        try:
            executable = os.readlink(proc / 'exe')
            if not (executable.startswith('/home/el01/binance_exec_')
                    or executable.startswith('/home/el01/crypto_cta_manager/bin/')
                    or (Path(executable.removesuffix(' (deleted)')).name.startswith('python')
                        and b'/home/el01/binance_exec_' in (proc / 'cmdline').read_bytes())
                    or (Path(executable).name == 'nginx' and executable.startswith('/home/el01/'))):
                continue
            ticks = (proc / 'stat').read_text().rsplit(')', 1)[1].split()[19]
            processes.append({'pid': int(proc.name), 'executable': executable, 'start_ticks': ticks})
        except (OSError, IndexError):
            continue
    return {
        'processes': sorted(processes, key=lambda p: p['pid']),
        'unit_pids': {unit: subprocess.check_output(['systemctl', '--user', 'show', unit,
                      '--property=MainPID', '--value'], text=True).strip()
                      for unit in ['crypto-cta-manager-web.service', 'crypto-cta-nginx.service', 'postgresql-cta.service']},
        'config_sha256': hashlib.sha256((root / 'config/cta-manager.toml').read_bytes()).hexdigest(),
        'runtime_sha256': hashlib.sha256((root / 'bin/cta_web').read_bytes()).hexdigest(),
        'trade_configs': {path: hashlib.sha256(Path(path).read_bytes()).hexdigest()
                          for path in config['funding']['trade_engine_configs']},
    }

expected={'processes': [{'pid': 27826, 'executable': '/home/el01/binance_exec_trade09/exec-pre-trade', 'start_ticks': '544169762'}, {'pid': 32918, 'executable': '/home/el01/binance_exec_trade10/exec-pre-trade', 'start_ticks': '710703245'}, {'pid': 61751, 'executable': '/home/el01/binance_exec_trade02/exec-pre-trade', 'start_ticks': '526591796'}, {'pid': 248620, 'executable': '/home/el01/binance_exec_trade06/persist_manager', 'start_ticks': '664623144'}, {'pid': 249008, 'executable': '/home/el01/binance_exec_trade06/trade_engine', 'start_ticks': '664623499'}, {'pid': 249235, 'executable': '/home/el01/binance_exec_trade06/account_monitor', 'start_ticks': '664623929'}, {'pid': 251045, 'executable': '/home/el01/binance_exec_trade06/exec-pre-trade', 'start_ticks': '664624289'}, {'pid': 286082, 'executable': '/usr/bin/python3.12', 'start_ticks': '664647614'}, {'pid': 339409, 'executable': '/home/el01/binance_exec_trade03/viz_server', 'start_ticks': '456164348'}, {'pid': 359587, 'executable': '/home/el01/binance_exec_trade03/trade_engine', 'start_ticks': '456272001'}, {'pid': 360186, 'executable': '/home/el01/binance_exec_trade03/exec-pre-trade', 'start_ticks': '456273482'}, {'pid': 462336, 'executable': '/home/el01/binance_exec_trade03/persist_manager', 'start_ticks': '291589762'}, {'pid': 463215, 'executable': '/usr/bin/python3.12 (deleted)', 'start_ticks': '291591551'}, {'pid': 467854, 'executable': '/home/el01/binance_exec_trade06/viz_server', 'start_ticks': '664762852'}, {'pid': 599009, 'executable': '/home/el01/binance_exec_trade03/account_monitor', 'start_ticks': '343474081'}, {'pid': 709851, 'executable': '/home/el01/binance_exec_trade07/viz_server', 'start_ticks': '586855500'}, {'pid': 810841, 'executable': '/home/el01/binance_exec_trade07/account_monitor', 'start_ticks': '586902275'}, {'pid': 816697, 'executable': '/home/el01/binance_exec_trade07/exec-pre-trade', 'start_ticks': '586904649'}, {'pid': 1783984, 'executable': '/home/el01/binance_exec_trade01/persist_manager', 'start_ticks': '673883042'}, {'pid': 1786535, 'executable': '/home/el01/binance_exec_trade01/trade_engine', 'start_ticks': '673883409'}, {'pid': 1787381, 'executable': '/home/el01/binance_exec_trade01/account_monitor', 'start_ticks': '673883860'}, {'pid': 1787579, 'executable': '/home/el01/binance_exec_trade01/exec-pre-trade', 'start_ticks': '673884221'}, {'pid': 1787774, 'executable': '/home/el01/binance_exec_trade01/viz_server', 'start_ticks': '673884592'}, {'pid': 1789675, 'executable': '/usr/bin/python3.12', 'start_ticks': '673885009'}, {'pid': 1873546, 'executable': '/home/el01/.local/opt/nginx/1.24.0-2ubuntu7.15/sbin/nginx', 'start_ticks': '699812044'}, {'pid': 2044879, 'executable': '/home/el01/binance_exec_trade07/persist_manager', 'start_ticks': '466031363'}, {'pid': 2046840, 'executable': '/usr/bin/python3.12', 'start_ticks': '466039550'}, {'pid': 2057015, 'executable': '/home/el01/binance_exec_trade07/trade_engine', 'start_ticks': '466075493'}, {'pid': 2107865, 'executable': '/home/el01/binance_exec_trade13/persist_manager', 'start_ticks': '699957738'}, {'pid': 2108097, 'executable': '/home/el01/binance_exec_trade13/trade_engine', 'start_ticks': '699958107'}, {'pid': 2111160, 'executable': '/home/el01/binance_exec_trade13/account_monitor', 'start_ticks': '699958575'}, {'pid': 2112358, 'executable': '/home/el01/binance_exec_trade13/exec-pre-trade', 'start_ticks': '699958946'}, {'pid': 2112979, 'executable': '/home/el01/binance_exec_trade13/viz_server', 'start_ticks': '699959328'}, {'pid': 2115007, 'executable': '/usr/bin/python3.12', 'start_ticks': '699959763'}, {'pid': 2428613, 'executable': '/home/el01/binance_exec_xy_lxy21_05/persist_manager', 'start_ticks': '674289059'}, {'pid': 2429637, 'executable': '/home/el01/binance_exec_xy_lxy21_05/trade_engine', 'start_ticks': '674289452'}, {'pid': 2431580, 'executable': '/home/el01/binance_exec_xy_lxy21_05/account_monitor', 'start_ticks': '674289899'}, {'pid': 2431783, 'executable': '/home/el01/binance_exec_xy_lxy21_05/exec-pre-trade', 'start_ticks': '674290264'}, {'pid': 2431966, 'executable': '/home/el01/binance_exec_xy_lxy21_05/viz_server', 'start_ticks': '674290630'}, {'pid': 2433882, 'executable': '/usr/bin/python3.12', 'start_ticks': '674291064'}, {'pid': 2605826, 'executable': '/home/el01/crypto_cta_manager/bin/cta_web', 'start_ticks': '721539109'}, {'pid': 2837558, 'executable': '/usr/bin/python3.12', 'start_ticks': '543300203'}, {'pid': 2837623, 'executable': '/home/el01/binance_exec_trade09/viz_server', 'start_ticks': '543300241'}, {'pid': 2975407, 'executable': '/home/el01/binance_exec_trade12/persist_manager', 'start_ticks': '543398114'}, {'pid': 2975809, 'executable': '/home/el01/binance_exec_trade12/trade_engine', 'start_ticks': '543398497'}, {'pid': 2977285, 'executable': '/home/el01/binance_exec_trade12/account_monitor', 'start_ticks': '543398933'}, {'pid': 2977453, 'executable': '/home/el01/binance_exec_trade12/exec-pre-trade', 'start_ticks': '543399293'}, {'pid': 2978132, 'executable': '/home/el01/binance_exec_trade12/viz_server', 'start_ticks': '543399659'}, {'pid': 2979284, 'executable': '/usr/bin/python3.12', 'start_ticks': '543400065'}, {'pid': 3493099, 'executable': '/home/el01/binance_exec_trade10/persist_manager', 'start_ticks': '680231098'}, {'pid': 3493317, 'executable': '/home/el01/binance_exec_trade10/trade_engine', 'start_ticks': '680231467'}, {'pid': 3495270, 'executable': '/home/el01/binance_exec_trade10/account_monitor', 'start_ticks': '680231944'}, {'pid': 3495679, 'executable': '/home/el01/binance_exec_trade10/viz_server', 'start_ticks': '680232697'}, {'pid': 3496588, 'executable': '/usr/bin/python3.12', 'start_ticks': '680233126'}, {'pid': 3500316, 'executable': '/home/el01/binance_exec_trade11/persist_manager', 'start_ticks': '680235562'}, {'pid': 3502216, 'executable': '/home/el01/binance_exec_trade11/trade_engine', 'start_ticks': '680235943'}, {'pid': 3502499, 'executable': '/home/el01/binance_exec_trade11/account_monitor', 'start_ticks': '680236404'}, {'pid': 3502702, 'executable': '/home/el01/binance_exec_trade11/exec-pre-trade', 'start_ticks': '680236777'}, {'pid': 3503307, 'executable': '/home/el01/binance_exec_trade11/viz_server', 'start_ticks': '680237158'}, {'pid': 3506049, 'executable': '/usr/bin/python3.12', 'start_ticks': '680237593'}, {'pid': 3607796, 'executable': '/home/el01/binance_exec_trade02/persist_manager', 'start_ticks': '526152126'}, {'pid': 3608197, 'executable': '/home/el01/binance_exec_trade02/trade_engine', 'start_ticks': '526152503'}, {'pid': 3608413, 'executable': '/home/el01/binance_exec_trade09/persist_manager', 'start_ticks': '543811833'}, {'pid': 3609731, 'executable': '/home/el01/binance_exec_trade09/trade_engine', 'start_ticks': '543812188'}, {'pid': 3609824, 'executable': '/home/el01/binance_exec_trade02/account_monitor', 'start_ticks': '526152937'}, {'pid': 3610146, 'executable': '/home/el01/binance_exec_trade02/viz_server', 'start_ticks': '526153644'}, {'pid': 3611395, 'executable': '/home/el01/binance_exec_trade09/account_monitor', 'start_ticks': '543812631'}, {'pid': 3611738, 'executable': '/usr/bin/python3.12', 'start_ticks': '526154057'}, {'pid': 3655127, 'executable': '/home/el01/.local/opt/nginx/1.24.0-2ubuntu7.15/sbin/nginx', 'start_ticks': '664088182'}, {'pid': 3705881, 'executable': '/home/el01/crypto_cta_manager/bin/cta_monitor (deleted)', 'start_ticks': '700924669'}, {'pid': 3708239, 'executable': '/home/el01/binance_exec_trade08/persist_manager', 'start_ticks': '700925178'}, {'pid': 3710395, 'executable': '/home/el01/binance_exec_trade08/trade_engine', 'start_ticks': '700925524'}, {'pid': 3710651, 'executable': '/home/el01/binance_exec_trade08/account_monitor', 'start_ticks': '700925951'}, {'pid': 3710736, 'executable': '/home/el01/binance_exec_trade08/exec-pre-trade', 'start_ticks': '700926295'}, {'pid': 3710805, 'executable': '/home/el01/binance_exec_trade08/viz_server', 'start_ticks': '700926645'}, {'pid': 3840899, 'executable': '/usr/bin/python3.12', 'start_ticks': '474927527'}, {'pid': 4002284, 'executable': '/usr/bin/python3.12 (deleted)', 'start_ticks': '336119324'}, {'pid': 4088916, 'executable': '/home/el01/binance_exec_trade04/persist_manager', 'start_ticks': '517645735'}, {'pid': 4089152, 'executable': '/home/el01/binance_exec_trade04/trade_engine', 'start_ticks': '517645765'}, {'pid': 4089308, 'executable': '/home/el01/binance_exec_trade04/account_monitor', 'start_ticks': '517645885'}, {'pid': 4089394, 'executable': '/home/el01/binance_exec_trade04/exec-pre-trade', 'start_ticks': '517645925'}, {'pid': 4089459, 'executable': '/home/el01/binance_exec_trade04/viz_server', 'start_ticks': '517645967'}], 'unit_pids': {'crypto-cta-manager-web.service': '2605826', 'crypto-cta-nginx.service': '3655127', 'postgresql-cta.service': '509018'}, 'config_sha256': '9a8ec9975d216f3664ecd0aa4e43bc1b911c37d535b3023bb5825bff4909196a', 'runtime_sha256': '3e3a015b716736f8416d2ff0ceed756531d0e148525551bf405877c084676b73', 'trade_configs': {'/home/el01/binance_exec_trade01/trade_engine.toml': 'b40c54f7627a6722ea5a3c99aaed4fd7db2e0add8d949054286ef8804cf18028', '/home/el01/binance_exec_trade02/trade_engine.toml': 'b40c54f7627a6722ea5a3c99aaed4fd7db2e0add8d949054286ef8804cf18028', '/home/el01/binance_exec_trade03/trade_engine.toml': 'b40c54f7627a6722ea5a3c99aaed4fd7db2e0add8d949054286ef8804cf18028', '/home/el01/binance_exec_trade04/trade_engine.toml': 'b40c54f7627a6722ea5a3c99aaed4fd7db2e0add8d949054286ef8804cf18028', '/home/el01/binance_exec_xy_lxy21_05/trade_engine.toml': 'b40c54f7627a6722ea5a3c99aaed4fd7db2e0add8d949054286ef8804cf18028', '/home/el01/binance_exec_trade06/trade_engine.toml': 'b40c54f7627a6722ea5a3c99aaed4fd7db2e0add8d949054286ef8804cf18028', '/home/el01/binance_exec_trade07/trade_engine.toml': 'b40c54f7627a6722ea5a3c99aaed4fd7db2e0add8d949054286ef8804cf18028', '/home/el01/binance_exec_trade08/trade_engine.toml': 'b40c54f7627a6722ea5a3c99aaed4fd7db2e0add8d949054286ef8804cf18028', '/home/el01/binance_exec_trade09/trade_engine.toml': 'b40c54f7627a6722ea5a3c99aaed4fd7db2e0add8d949054286ef8804cf18028', '/home/el01/binance_exec_trade10/trade_engine.toml': 'b40c54f7627a6722ea5a3c99aaed4fd7db2e0add8d949054286ef8804cf18028', '/home/el01/binance_exec_trade11/trade_engine.toml': 'b40c54f7627a6722ea5a3c99aaed4fd7db2e0add8d949054286ef8804cf18028', '/home/el01/binance_exec_trade12/trade_engine.toml': 'b40c54f7627a6722ea5a3c99aaed4fd7db2e0add8d949054286ef8804cf18028', '/home/el01/binance_exec_trade13/trade_engine.toml': '2745a205315b7935c77a5e68eb0e6a3078189be2e77db6f9231e63cb2eefc6a8'}}

def get(path):
    connection=http.client.HTTPConnection('127.0.0.1',10051,timeout=10)
    try:
        connection.request('GET',path)
        response=connection.getresponse()
        content=response.read()
        assert response.status==200,(path,response.status)
        return content
    finally:
        connection.close()
page=get('/manager/?source=binance_exec_trade09').decode()
assets=re.findall(r'(?:src|href)="(/manager/assets/[^"]+)"',page)
assert len(assets)>=2
for asset in assets:
    content=get(asset)
    if asset.endswith('.js'):
        for label in ['资金费明细','按币种查看观测进度']:
            assert label.encode() not in content,label
        for label in ['Funding（累计资金费）','区间 Funding（资金费）','账户资金费与总体盈亏']:
            assert label.encode() in content,label
assert runtime_snapshot()==expected,'runtime or trading config changed'
print(json.dumps({'frontend_release':os.readlink(root/'webroot'),'assets':assets,
                  'detail_sections_removed':True,'funding_curve_and_summary_preserved':True,
                  'processes_unchanged':len(expected['processes']),
                  'trade_configs_unchanged':len(expected['trade_configs']),
                  'manager_restarted':False,'utc':datetime.datetime.now(datetime.timezone.utc).isoformat()},ensure_ascii=False))
