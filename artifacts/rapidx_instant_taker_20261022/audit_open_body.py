# Append to remote_common.py. Read only the Manager candle cache and fee catalog.
import math
stage = ROOT/'deploy-staging/rapidx_instant_taker_20261022'
data = json.loads((stage/'after.json').read_text())
fees = {v['source_id']: v for v in json.loads(sql("SELECT json_agg(r) FROM (SELECT source_id,taker_fee_rate,quote_fee_rates FROM cta_order_sources) r;"))}
rows = [row for key, body in data['queries'].items() if key.endswith('_cost') for report in body['reports'] if report['execution_model'] == 'instant_taker' for row in report['rows']]
symbols = sorted({r['symbol'] for r in rows})
timestamps = sorted({(r['received_at_us']//60_000_000+1)*60_000_000 for r in rows})
binary = stage/'nav_cached_marks'
os.chmod(binary, 0o755)
command = [str(binary), '--manager-db', str(cfg['kline']['rocksdb_path'])]
command += [v for symbol in symbols for v in ['--symbol', symbol]]
command += [v for ts in timestamps for v in ['--ts-us', str(ts)]]
result = subprocess.run(command, text=True, capture_output=True, check=True)
candles = {(r['symbol'], r['open_ts_us']): r for r in map(json.loads, result.stdout.splitlines())}
summary = {}
for row in rows:
    minute = row['received_at_us']//60_000_000*60_000_000
    candle = candles[row['symbol'], minute]
    assert row['virtual_vwap'] == candle['open'], (row['source_id'], row['symbol'], 'open mismatch')
    assert row['sample_prices'] == [candle['open']]
    account = fees[row['source_id']]
    quote = 'USDC' if row['symbol'].endswith('USDC') else 'USDT'
    rate = account['quote_fee_rates'].get(quote, {}).get('taker_fee_rate', account['taker_fee_rate'])
    fee = abs(row['delta_qty'])*candle['open']*rate
    assert math.isclose(row['virtual_fee_usdt'], fee, rel_tol=1e-12, abs_tol=1e-9)
    assert row['virtual_execution_ts_us'] == row['received_at_us']
    own = next(r['price'] for r in row['reference_prices'] if r['execution_model'] == 'instant_taker')
    assert own == candle['open']
    source = summary.setdefault(row['source_id'], {'rows': 0, 'open_differs_from_close': 0, 'fee_verified': 0, 'buy': 0, 'sell': 0})
    source['rows'] += 1
    source['open_differs_from_close'] += candle['open'] != candle['close']
    source['fee_verified'] += 1
    source['buy' if row['delta_qty'] > 0 else 'sell'] += 1
assert rows and any(v['open_differs_from_close'] for v in summary.values())
emit('independent_open_and_taker_fee', summary)
(stage/'open_audit.json').write_text(json.dumps(summary, indent=2)+'\n')
