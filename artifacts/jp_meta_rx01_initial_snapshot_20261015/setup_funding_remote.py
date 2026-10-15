"""Explicit additive Manager schema/config maintenance for the three LGQ sources."""
import json
import os
import pathlib
import secrets
import subprocess
import tomllib
import urllib.parse

ROOT=pathlib.Path('/home/ubuntu/crypto_cta_manager')
STAGE=ROOT/'backups/rx01_initial_snapshot_20261015'
SOURCES=['binance_exec_trade05','binance_exec_trade06','binance_exec_trade07']
pid=int(subprocess.check_output(['systemctl','--user','show','crypto-cta-manager-web.service','-p','MainPID','--value'],text=True))
runtime=dict(x.split(b'=',1) for x in pathlib.Path(f'/proc/{pid}/environ').read_bytes().split(b'\0') if b'=' in x)
u=urllib.parse.urlsplit(runtime[b'CRYPTO_CTA_LOCAL_DATABASE_URL'].decode())
env=dict(os.environ,PGHOST=u.hostname,PGPORT=str(u.port or 5432),PGUSER=urllib.parse.unquote(u.username),
         PGPASSWORD=urllib.parse.unquote(u.password or ''),PGDATABASE=u.path.lstrip('/'))
def sql(q):
    r=subprocess.run(['psql','-X','-qAt','-v','ON_ERROR_STOP=1'],input=q,text=True,capture_output=True,env=env)
    assert r.returncode==0,'Manager schema maintenance failed (details suppressed)'
    return r.stdout.strip()

assert sql("SELECT count(*) FROM information_schema.tables WHERE table_schema='public' AND table_name IN ('cta_funding_accounts','cta_funding_symbols','cta_funding_income');")=='0'
assert sql("SELECT count(*) FROM information_schema.columns WHERE table_name='cta_order_sources' AND column_name='funding_enabled';")=='0'
path=ROOT/'config/cta-manager.toml'
original=path.read_text(); cfg=tomllib.loads(original)
assert 'funding' not in cfg
probe=json.loads((STAGE/'funding_probe.json').read_text())
assert {p['source_id'] for p in probe}==set(SOURCES)
assert all(p['http_status']==200 and p['local_ip']=='172.31.35.228' and p['public_ip']=='13.115.227.29' for p in probe)
for source_id in SOURCES:
    source=next(s for s in cfg['sources'] if s['id']==source_id)
    assert source['enabled'] and source['venue']=='binance-futures'
    engine=pathlib.Path(source['rocksdb_path']).parent.parent/'trade_engine.toml'
    assert tomllib.loads(engine.read_text())['local_ips'][0]=='172.31.35.228'
schema='\n'.join((STAGE/name).read_text().replace('BEGIN;','').replace('COMMIT;','') for name in ['apply_funding_schema.sql','apply_funding_settings_schema.sql'])
with (STAGE/'source_catalog_schema_before.sql').open('w') as out:
    r=subprocess.run(['pg_dump','--schema-only','--no-owner','--no-privileges','--table','cta_order_sources'],stdout=out,stderr=subprocess.PIPE,env=env)
    assert r.returncode==0,'catalog schema backup failed'
(STAGE/'cta-manager.before-funding.toml').write_text(original)
sql('BEGIN;\n'+schema+'\nCOMMIT;')
assert sql('SELECT count(*) FROM cta_order_sources WHERE funding_enabled;')=='0'
funding={'local_ip':'172.31.35.228','public_ip':'13.115.227.29',
         'trade_engine_configs':cfg['kline']['trade_engine_configs'],
         'forbidden_public_ips':[], 'rapidx_trading_ip_sources':SOURCES,
         'enable_token_env':'CTA_FUNDING_ENABLE_TOKEN'}
addition='\n[funding]\n# Operator-authorized LGQ RapidX trading-IP exception, 2026-10-15.\n'
addition+='\n'.join(k+' = '+json.dumps(v) for k,v in funding.items())+'\n'
updated=original+addition
parsed=tomllib.loads(updated); assert parsed.pop('funding')==funding and parsed==cfg
next_path=path.with_name('cta-manager.toml.next-funding')
next_path.write_text(updated); next_path.chmod(path.stat().st_mode & 0o777); os.replace(next_path,path)
environment_file=pathlib.Path('/home/ubuntu/.config/crypto-cta-manager/database.env')
body=environment_file.read_text()
if not any(line.startswith('CTA_FUNDING_ENABLE_TOKEN=') for line in body.splitlines()):
    with environment_file.open('a') as out: out.write('\nCTA_FUNDING_ENABLE_TOKEN='+secrets.token_urlsafe(48)+'\n')
    environment_file.chmod(0o600)
print(json.dumps({'schema_tables_created':3,'settings_columns_added':5,'funding_sources':SOURCES,
                  'fixed_local_ip':funding['local_ip'],'expected_public_ip':funding['public_ip'],'other_accounts_enabled':False}))
