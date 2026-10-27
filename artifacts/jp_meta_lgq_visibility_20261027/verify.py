from pathlib import Path
import subprocess

out = Path(__file__).resolve().parent
common = (out.parent / 'jp_meta_mm_exec_overview_20261027/host_common.py').read_text()
common = common.replace("'http://127.0.0.1:4191/manager/api/' + path",
                        "'http://127.0.0.1:4191' + (path if path.startswith('/') else '/manager/api/' + path)")
common = common.replace('return json.load(r)', "return json.load(r) if 'json' in r.headers.get('Content-Type','') else {'http_status':r.status}")
script = common + '\n' + (out / 'verify_remote.py').read_text()
with (out / 'after.jsonl').open('w') as log:
    result = subprocess.run(['ssh', 'jp-meta-elvpn', 'python3 -'], input=script, text=True, stdout=log, stderr=subprocess.PIPE)
if result.returncode:
    print(result.stderr)
    raise SystemExit(result.returncode)
print('Both user views, direct Exec gateways, totals, history and anonymous restrictions passed.')
