import json
from pathlib import Path

out = Path(__file__).resolve().parent
read = lambda p: [json.loads(line) for line in p.read_text().splitlines()]
find = lambda rows, key: next(row['body'] for row in rows if row['name'] == key)
summary = {}
for host in ['sg', 'jp-meta-elvpn']:
    before = find(read(out / (host + '-before.jsonl')), 'protected')
    after_rows = read(out / (host + '-verified.jsonl'))
    after = find(after_rows, 'protected')
    changed = {pid: value for pid, value in before['processes'].items() if after['processes'].get(pid) != value}
    assert not changed, (host, changed)
    for name, value in before['config_hashes'].items():
        if name.endswith('/crypto_cta_manager/config/cta-manager.toml'):
            continue
        assert after['config_hashes'][name] == value, (host, name)
    for name, value in before['tables'].items():
        assert after['tables'][name] == value, (host, name, value, after['tables'][name])
    assert before['manager_affinity'] == after['manager_affinity']
    if host == 'sg':
        assert after['tables']['cta_strategy_allocation_snapshots']['count'] == 0
        assert before['webroot'] == after['webroot']
    summary[host] = {'protected_processes': len(before['processes']),
                     'unchanged_configurations_excluding_planned_toml': len(before['config_hashes']) - 1,
                     'unchanged_tables': {name: value['count'] for name, value in before['tables'].items()},
                     'manager_affinity': after['manager_affinity']}
jp = read(out / 'jp-meta-elvpn-verified.jsonl')
accounts = [r for r in find(jp, 'lgq_overview')['accounts'] if r['source_id'].startswith('external:')]
summary['visibility'] = {'lgq_external_count': len(accounts), 'prc_external_count': 0,
                         'account_kinds': {kind: sum(r['account_kind'] == kind for r in accounts) for kind in ['fr', 'mm', 'cta']}}
assert find(jp, 'prc_external_access_empty')
manifest = json.loads((out / 'manifest.json').read_text())
assets = {r['body']['path']: r['body']['sha256'] for r in jp if r['name'] == 'asset'}
for name, value in manifest['frontend'].items():
    if name.startswith('assets/'):
        assert assets['/manager/' + name] == value
(out / 'comparison.json').write_text(json.dumps(summary, indent=2) + '\n')
print(json.dumps(summary, indent=2))
