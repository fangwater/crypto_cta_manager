"""Pass an ephemeral session over SSH stdin, then delete it in finally."""
import json
from pathlib import Path
import subprocess

artifact = Path(__file__).resolve().parent
source = (artifact / 'probe_remote.py').read_text()
header = source[:source.index('\ntoken = secrets.token_urlsafe')]
create = source[:source.index('\nopener = urllib.request.build_opener')]
create += '\nprint(json.dumps({"token":token,"session_id":session_id}))\n'
created = subprocess.run(['ssh', '-o', 'BatchMode=yes', 'cta_exec', 'python3', '-'],
                         input=create, text=True, capture_output=True, check=True)
session = json.loads(created.stdout)
assert session['session_id'].isdigit()
try:
    result = subprocess.run(['ssh', '-o', 'BatchMode=yes', 'el_dev', 'node',
                             '/home/fanghaizhou/snap/chromium/common/cta-30d-fix-20261015/browser_live_check.mjs',
                             '/home/fanghaizhou/snap/chromium/common/cta-30d-fix-20261015'],
                            input=json.dumps(session), text=True, capture_output=True)
    (artifact / 'browser_live_result.json').write_text(result.stdout)
    if result.returncode:
        raise RuntimeError(result.stderr)
    parsed = json.loads(result.stdout)
    print(json.dumps({'passed': parsed['passed'], 'viewports': len(parsed['results'])}))
finally:
    cleanup = header + '\nprint(sql("WITH removed AS (DELETE FROM cta_auth_sessions WHERE session_id=' + session['session_id'] + ' RETURNING session_id) SELECT count(*) FROM removed;"))\n'
    removed = subprocess.run(['ssh', '-o', 'BatchMode=yes', 'cta_exec', 'python3', '-'],
                            input=cleanup, text=True, capture_output=True, check=True)
    assert removed.stdout.strip() == '1', 'diagnostic session was not removed'
    print(json.dumps({'verification_session_removed': True}))
