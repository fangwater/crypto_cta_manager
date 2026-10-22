import json,subprocess,sys
from pathlib import Path
out=Path(__file__).resolve().parent
phase=sys.argv[1]
assert phase in ['before','after']
remote=(out/'remote_common.py').read_text()+"\n"+(out/'nav_body.py').read_text()
with (out/('nav_'+phase+'.jsonl')).open('w') as log:
    p=subprocess.Popen(['ssh','cta_exec','python3','-',phase],stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True)
    p.stdin.write(remote);p.stdin.close()
    for line in p.stdout:
        print(line,end='',flush=True);log.write(line);log.flush()
    raise SystemExit(p.wait())
