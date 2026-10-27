"""Read-only deployment comparisons through el01 Nginx, with disposable auth."""
import json
from pathlib import Path
import subprocess
import sys
import time

OUT = Path(__file__).resolve().parent
phase = sys.argv[1]
common = (OUT.parent / 'el01_multiple_virtuals_20261022/remote_common.py').read_text()
common = common.replace("'cta_strategy_position_snapshot_entries']", "'cta_strategy_position_snapshot_entries', 'cta_strategy_allocation_snapshots', 'cta_account_equity_daily']")
window_file = OUT / 'window.json'
if not window_file.exists():
    end = (int(time.time()) // 3600 - 2) * 3600000
    window_file.write_text(json.dumps(dict(startMs=end-86400000, endMs=end, maxPoints=100, includeTheoretical='false'))+'\n')
body = '\nwindow = ' + repr(json.loads(window_file.read_text())) + '\n' + r'''
emit('protected', protected())
with gateway() as get:
    emit('health', get('health'))
    dashboard = get('dashboard')
    emit('accounts', {a['source_id']: a.get('cumulative_funding') for a in dashboard['accounts'] if a['enabled']})
    for source in SOURCES:
        for endpoint in ['timeline', 'account-timeline']:
            scoped = dict(window, sourceIds=source)
            began = time.monotonic()
            while True:
                try:
                    result = get(endpoint+'?'+urllib.parse.urlencode(scoped))
                    break
                except urllib.error.HTTPError as error:
                    reason = json.loads(error.read())
                    if error.code == 503 and reason.get('loading') and time.monotonic()-began < 600:
                        time.sleep(10)
                        continue
                    emit('query_error', dict(source=source, endpoint=endpoint, http=error.code, reason=reason))
                    result = None
                    break
            if result is not None:
                report = result['report']
                emit('query', dict(source=source, endpoint=endpoint, report_sha256=digest(report), summary=report['summary'], valuation=report.get('valuation'), points=len(report['points'])))
'''
with (OUT / (phase+'.jsonl')).open('w') as log:
    p = subprocess.Popen(['ssh','-o','BatchMode=yes','cta_exec','python3','-'], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    p.stdin.write(common+body)
    p.stdin.close()
    for line in p.stdout:
        log.write(line)
        log.flush()
        row = json.loads(line)
        if row['name'] == 'protected':
            print('Protected baseline: '+str(len(row['body']['processes']))+' processes', flush=True)
        else:
            print(line,end='',flush=True)
    raise SystemExit(p.wait())
