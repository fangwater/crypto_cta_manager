"""Run one explicitly selected maintenance/verification phase on jp-meta."""
from pathlib import Path
import subprocess
import sys

phase = sys.argv[1]
assert phase in ['before', 'apply', 'after', 'protected', 'install-binary', 'status', 'sample', 'separate-egress', 'tighten-budget', 'cleanup', 'funding-health', 'concurrent-maintenance']
root = Path(__file__).resolve().parent
code = (root/'remote_common.py').read_text() + '\n' + (root/'operation_body.py').read_text()
with (root/(phase+'.jsonl')).open('w') as output:
    process = subprocess.Popen(['ssh', '-o', 'BatchMode=yes', 'jp-meta', 'python3', '-', phase],
                               stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    process.stdin.write(code)
    process.stdin.close()
    for line in process.stdout:
        output.write(line)
        output.flush()
        print(line, end='', flush=True)
    raise SystemExit(process.wait())
