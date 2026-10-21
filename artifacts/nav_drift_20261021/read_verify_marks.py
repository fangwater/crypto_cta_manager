import json,subprocess,tomllib
from pathlib import Path
root=Path('/home/el01/crypto_cta_manager')
c=tomllib.loads((root/'config/cta-manager.toml').read_text())
s=next(s for s in c['sources'] if s['id']=='binance_exec_trade01')
symbols='1000PEPEUSDT AAVEUSDT ADAUSDT BNBUSDT BTCUSDT DOGEUSDT ETHUSDT NEARUSDT SOLUSDT SUIUSDT UNIUSDT WLDUSDT XRPUSDT'.split()
args=[str(root/'nav_marks_verify_20261021/nav_cached_marks'),'--manager-db',c.get('kline',{}).get('rocksdb_path',str(root/'db'))]
for symbol in symbols:args+=['--symbol',symbol]
for ts in [1789976400000000]:args+=['--ts-us',str(ts)]
subprocess.run(args,check=True)
