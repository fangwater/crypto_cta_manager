"""Compare saved, sanitized baseline and final verification records."""
import json
from pathlib import Path

root = Path(__file__).resolve().parent
read = lambda name: [json.loads(line) for line in (root / name).read_text().splitlines()]
before, after = read('before.jsonl'), read('after.jsonl')
get = lambda records, name: next(row['body'] for row in records if row['name'] == name)
old, new = get(before, 'protected'), get(after, 'protected')
assert old['config_hashes'] == new['config_hashes']
assert old['tables'] == new['tables']
assert old['funding_settings'] == new['funding_settings']
assert old['manager_affinity'] == new['manager_affinity']
assert get(after, 'diagnostic_session_removed')
assert get(after, 'historical_exposure_unknown')
ignored = {'crypto_nav_manager', 'sync_history'}
protected = {pid: state for pid, state in old['processes'].items()
             if Path(state['exe']).name not in ignored}
changed = {pid: state for pid, state in protected.items() if new['processes'].get(pid) != state}
assert not changed, changed
upstream = get(after, 'nav_after')
assert {row['source_id'] for row in upstream['accounts']} == {
    row['source_id'] for row in get(before, 'nav_before')['accounts']}
manifest = json.loads((root / 'manifest.json').read_text())
assets = {row['body']['path']: row['body']['sha256'] for row in after if row['name'] == 'asset'}
for path, value in manifest['frontend'].items():
    if path.startswith('assets/'):
        assert assets['/manager/' + path] == value
result = {'protected_process_count': len(protected), 'configuration_hash_count': len(old['config_hashes']),
          'unchanged_tables': {name: data['count'] for name, data in old['tables'].items()},
          'manager_affinity': new['manager_affinity'], 'account_count': len(upstream['accounts']),
          'diagnostic_session_removed': True, 'historical_exposure_unknown': True,
          'frontend_assets_match': True}
(root / 'comparison.json').write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps(result, indent=2))
