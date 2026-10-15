"""Run API and Chromium checks while keeping temporary cookies off tool output."""
import json
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parent
REMOTE = '/home/el01/crypto_cta_manager/deploy-staging/position-creation-20261014T093305Z/verify_remote.py'
process = subprocess.Popen(['ssh', '-o', 'BatchMode=yes', 'cta_exec',
                            'python3 ' + REMOTE + ' after --browser'],
                           stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                           stderr=subprocess.PIPE, text=True)
line = process.stdout.readline()
if not line:
    _, errors = process.communicate()
    print('API verification failed before browser checks; no session data logged', file=sys.stderr)
    print(errors, file=sys.stderr)
    raise SystemExit(1)
payload = json.loads(line)
assert set(payload) == {'users'}, 'unexpected verification handshake'
browser = subprocess.run(['node', str(ROOT / 'verify_browser.cjs')],
                         input=line, text=True, capture_output=True)
if browser.returncode == 0:
    result = json.loads(browser.stdout)
else:
    result = {'ok': False}
    print(browser.stderr, file=sys.stderr)
output, errors = process.communicate(json.dumps(result) + '\n')
print(output.strip())
if process.returncode or browser.returncode:
    print(errors, file=sys.stderr)
    raise SystemExit(1)
