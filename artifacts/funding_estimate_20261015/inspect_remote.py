import json,os,pathlib,subprocess,urllib.parse
root=pathlib.Path.home()/'crypto_cta_manager'
pid=int(subprocess.check_output(['systemctl','--user','show','crypto-cta-manager-web.service','-p','MainPID','--value'],text=True))
runtime=dict(x.split(b'=',1) for x in pathlib.Path(f'/proc/{pid}/environ').read_bytes().split(b'\0') if b'=' in x)
u=urllib.parse.urlsplit(runtime[b'CRYPTO_CTA_LOCAL_DATABASE_URL'].decode())
env=dict(os.environ,PGHOST=u.hostname,PGPORT=str(u.port or 5432),PGUSER=urllib.parse.unquote(u.username),PGPASSWORD=urllib.parse.unquote(u.password or ''),PGDATABASE=u.path.lstrip('/'))
psql='/home/el01/.local/opt/postgresql/16/bin/psql' if pathlib.Path.home().name=='el01' else 'psql'
q="""SELECT coalesce(jsonb_agg(to_jsonb(t)),'[]') FROM (
SELECT s.source_id,s.symbol,s.start_ts_us,s.synced_through_ts_us,count(i.*) AS income_count,min(i.ts_us) AS first_income_ts,max(i.ts_us) AS last_income_ts
FROM cta_funding_symbols s LEFT JOIN cta_funding_income i USING(source_id,venue,symbol)
GROUP BY s.source_id,s.symbol,s.start_ts_us,s.synced_through_ts_us ORDER BY s.source_id,s.symbol) t;"""
r=subprocess.run([psql,'-X','-qAt','-v','ON_ERROR_STOP=1'],input=q,text=True,capture_output=True,env=env)
assert r.returncode==0,'read failed (details suppressed)'
print(r.stdout.strip())
