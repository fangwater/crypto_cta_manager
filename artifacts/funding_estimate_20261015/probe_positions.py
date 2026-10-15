"""Extract only financial position fields from existing read-only Exec journals."""
import json
import pathlib

root = pathlib.Path('/home/ubuntu/crypto_cta_manager/backups/funding_estimate_20261015')
reports = json.loads((root / 'comparison.json').read_text())['reports']
windows = {}
for report in reports:
    if report['allocation']:
        continue
    windows[report['source_id']] = {(r['symbol'], r['ts_us'] // 60_000_000 * 60_000_000)
        for r in report['comparisons'] if r['observed'] and r['actual']}
out = []
keys = ['sym', 'positionQty', 'positionSide', 'markPrice', 'fundingFee', 'createAt', 'updateAt']
counts = {}
for source, intervals in windows.items():
    scanned = 0
    journal_root = pathlib.Path('/home/ubuntu') / source / 'data/rapidx_account'
    for path in sorted(journal_root.rglob('*.jsonl')):
        with path.open() as stream:
            for line in stream:
                scanned += 1
                if 'Positions' not in line:
                    continue
                try:
                    record = json.loads(line)
                except json.JSONDecodeError:
                    # The active journal's last append may not be complete yet.
                    continue
                message = record.get('message', {})
                if message.get('channel') != 'Positions':
                    continue
                received = record.get('received_us', 0)
                rows = message.get('data', [])
                if isinstance(rows, dict):
                    rows = [rows]
                for row in rows:
                    parts = row.get('sym', '').split('_')
                    if len(parts) != 4 or parts[:2] != ['BINANCE', 'PERP']:
                        continue
                    symbol = parts[2] + parts[3]
                    for target, ts in intervals:
                        if target == symbol and ts - 60_000_000 <= received <= ts + 300_000_000:
                            out.append({'source_id': source, 'settlement_us': ts, 'received_us': received,
                                        'row': {k: row.get(k) for k in keys}})
                            break
    counts[source] = scanned
(root / 'rapidx_positions.json').write_text(json.dumps({'scanned_lines': counts, 'positions': out}, indent=2) + '\n')
print(json.dumps({'scanned_lines': counts, 'position_count': len(out)}))
