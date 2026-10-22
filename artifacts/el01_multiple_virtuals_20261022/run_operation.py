import subprocess,sys
from pathlib import Path
root=Path(__file__).resolve().parent
phase=sys.argv[1]
assert phase in ['before','maintain','after','cleanup']
script=(root/'remote_common.py').read_text()+'\n'+(root/'operation_body.py').read_text()
with (root/(phase+'.jsonl')).open('w') as log:
    process=subprocess.Popen(['ssh','-o','BatchMode=yes','cta_exec','python3','-',phase],stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True)
    process.stdin.write(script); process.stdin.close()
    for line in process.stdout:
        print(line,end='',flush=True); log.write(line); log.flush()
    raise SystemExit(process.wait())
