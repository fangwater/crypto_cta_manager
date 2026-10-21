import json,subprocess,tomllib
from pathlib import Path
root=Path('/home/el01/crypto_cta_manager')
c=tomllib.loads((root/'config/cta-manager.toml').read_text())
s=next(s for s in c['sources'] if s['id']=='binance_exec_trade01')
symbols='1000PEPEUSDT AAVEUSDT ADAUSDT BNBUSDT BTCUSDT DOGEUSDT ETHUSDT NEARUSDT SOLUSDT SUIUSDT UNIUSDT WLDUSDT XRPUSDT'.split()
args=[str(root/'nav_drift_20261021_checks/nav_cached_marks'),'--manager-db',c.get('kline',{}).get('rocksdb_path',str(root/'db')),'--ts-us','1789970112413000']
for symbol in symbols:args+=['--symbol',symbol]
subprocess.run(args,check=True)
subprocess.run([str(root/'nav_drift_20261021_checks/nav_symbol_fills'),'--rocksdb-path',s['rocksdb_path'],'--verbose'],check=True)
