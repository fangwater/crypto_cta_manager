import json, os, subprocess, tomllib, urllib.parse, urllib.request
from pathlib import Path
ROOT=Path('/home/el01/crypto_cta_manager')
SOURCE='binance_exec_trade14'
cfg=tomllib.loads((ROOT/'config/cta-manager.toml').read_text())
pid=subprocess.check_output(['systemctl','--user','show','crypto-cta-manager-web.service','-p','MainPID','--value'],text=True).strip()
runtime=dict(x.split(b'=',1) for x in Path('/proc/'+pid+'/environ').read_bytes().split(b'\0') if b'=' in x)
u=urllib.parse.urlsplit(runtime[cfg.get('database',{}).get('url_env','CRYPTO_CTA_LOCAL_DATABASE_URL').encode()].decode())
env=dict(os.environ,PGHOST=u.hostname,PGPORT=str(u.port or 5432),PGUSER=urllib.parse.unquote(u.username),PGPASSWORD=urllib.parse.unquote(u.password or ''),PGDATABASE=urllib.parse.unquote(u.path.lstrip('/')),PGCONNECT_TIMEOUT='10',PGOPTIONS='-c default_transaction_read_only=on -c statement_timeout=15000')
def sql(query):
 r=subprocess.run([str(Path.home()/'.local/opt/postgresql/16/bin/psql'),'-X','-q','-A','-t','-v','ON_ERROR_STOP=1'],input=query,text=True,capture_output=True,env=env)
 if r.returncode: raise RuntimeError('read-only database query failed; credentials suppressed')
 return json.loads(r.stdout)
def emit(k,v): print(json.dumps({'name':k,'body':v},ensure_ascii=False),flush=True)
source=next(s for s in cfg['sources'] if s['id']==SOURCE)
emit('source',{k:v for k,v in source.items() if k in ['id','account','account_name','enabled','rocksdb_path','viz_url','viz_snapshot_url','venue']})
for table in ['cta_position_snapshots','cta_position_snapshot_entries','cta_strategy_position_snapshots','cta_strategy_position_snapshot_entries']:
 emit(table,sql("SELECT coalesce(json_agg(t),'[]'::json) FROM (SELECT * FROM "+table+" WHERE source_id='"+SOURCE+"') t;"))
emit('bindings',sql("SELECT coalesce(json_agg(t),'[]'::json) FROM (SELECT position_strategy_name,source_id,shares,order_strategy_name FROM cta_account_strategy_bindings WHERE source_id='"+SOURCE+"') t;"))
opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
try:
 with opener.open('http://127.0.0.1:10055/snapshot',timeout=15) as r: emit('exec_snapshot',json.load(r))
except Exception as e: emit('exec_snapshot_error',type(e).__name__)
procs=[]
for p in Path('/proc').iterdir():
 if not p.name.isdigit():continue
 try:
  cwd=str((p/'cwd').resolve()); exe=str((p/'exe').resolve())
  if cwd=='/home/el01/'+SOURCE:
   procs.append({'pid':int(p.name),'exe':exe,'stdout':os.readlink(p/'fd/1'),'stderr':os.readlink(p/'fd/2')})
 except OSError:pass
emit('processes',procs)
emit('manager_pid',int(pid))
