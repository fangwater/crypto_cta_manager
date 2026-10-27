import json
import gzip
from pathlib import Path
import subprocess
import sys

out = Path(__file__).resolve().parent
host = sys.argv[1]
assert host in ['sg', 'jp-meta-elvpn']
with gzip.open(out / 'sg-fixed-before.jsonl.gz', 'rt') as evidence:
    fixed = [json.loads(line)['body'] for line in evidence if json.loads(line)['name'] == 'fixed_query']
script = (out / 'host_common.py').read_text() + '\nHOST=' + repr(host) + '\nFIXED=' + repr(fixed) + '\n' + (out / 'verify_remote.py').read_text()
with (out / (host + '-verified.jsonl')).open('w') as log:
    p = subprocess.Popen(['ssh', host, 'python3', '-u', '-'], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    p.stdin.write(script)
    p.stdin.close()
    for line in p.stdout:
        log.write(line)
        log.flush()
        record = json.loads(line)
        if record['name'] in ['lgq_overview', 'nav_provider', 'sg_provider', 'sg_export']:
            print(record['name'], [(r['source_id'], r.get('account_kind'), r.get('collection_status')) for r in record['body']['accounts']], flush=True)
        elif record['name'] != 'protected':
            print(line, end='', flush=True)
    p.wait()
    if p.returncode:
        print(p.stderr.read())
    print(host, 'verification_exit', p.returncode)
    raise SystemExit(p.returncode)
