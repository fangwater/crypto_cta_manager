import json
from pathlib import Path

out = Path(__file__).resolve().parent


def records(name):
    return {r['name']: r['body'] for r in map(json.loads, (out / name).read_text().splitlines())}


before, after = records('before.jsonl'), records('after.jsonl')
m = json.loads((out / 'manifest.json').read_text())
a, b = before['protected'], after['protected']
assert a['processes'] == b['processes'], 'protected process identity/affinity changed'
assert a['tables'] == b['tables'], 'protected snapshot/catalog tables changed'
config = '/home/ubuntu/crypto_cta_manager/config/cta-manager.toml'
assert {k: v for k, v in a['config_hashes'].items() if k != config} == {k: v for k, v in b['config_hashes'].items() if k != config}
assert a['webroot'] == b['webroot']
assert a['manager_affinity'] == b['manager_affinity']
assert b['binary_sha256'] == m['binaries']['cta_web']
assert after['visibility_config'] == {'source_viewers': m['source_viewers'], 'excluded_source_ids': m['excluded']}
assert before['grants_hash'] == after['grants_hash'], 'account permissions changed'
result = {'protected_processes': len(a['processes']), 'unchanged_config_files': len(a['config_hashes']) - 1,
          'unchanged_tables': len(a['tables']), 'manager_affinity': b['manager_affinity'],
          'grants_unchanged': True, 'frontend_unchanged': True, 'binary_verified': True}
(out / 'comparison.json').write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps(result))
