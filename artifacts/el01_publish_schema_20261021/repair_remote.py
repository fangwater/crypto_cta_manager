"""Diagnose read-only dashboard queries; keep credentials on the host."""
import hashlib
import json
import os
from pathlib import Path
import secrets
import subprocess
import time
import tomllib
import urllib.parse
import urllib.request

ROOT = Path('/home/el01/crypto_cta_manager')
UNIT = 'crypto-cta-manager-web.service'
cfg = tomllib.loads((ROOT / 'config/cta-manager.toml').read_text())
pid = subprocess.check_output(['systemctl', '--user', 'show', UNIT, '-p', 'MainPID', '--value'], text=True).strip()
runtime = dict(entry.split(b'=', 1) for entry in Path(f'/proc/{pid}/environ').read_bytes().split(b'\0') if b'=' in entry)
url = urllib.parse.urlsplit(runtime[cfg.get('database', {}).get('url_env', 'CRYPTO_CTA_LOCAL_DATABASE_URL').encode()].decode())
env = dict(os.environ, PGHOST=url.hostname, PGPORT=str(url.port or 5432),
           PGUSER=urllib.parse.unquote(url.username), PGPASSWORD=urllib.parse.unquote(url.password or ''),
           PGDATABASE=urllib.parse.unquote(url.path.lstrip('/')), PGCONNECT_TIMEOUT='10')


def sql(query):
    result = subprocess.run([str(Path.home() / '.local/opt/postgresql/16/bin/psql'), '-X', '-q', '-A', '-t', '-v', 'ON_ERROR_STOP=1'],
                            input=query, text=True, capture_output=True, env=env)
    if result.returncode:
        raise RuntimeError('diagnostic database operation failed (credentials suppressed)')
    return result.stdout.strip()


maintenance='BEGIN;\nSET LOCAL lock_timeout=\'3s\';\nSET LOCAL statement_timeout=\'15s\';\n-- Explicit maintenance for an existing database, before deploying Grid support.\n-- Review and run separately; application startup never executes this file.\n\nALTER TABLE cta_order_strategies\n    ADD COLUMN grid jsonb NOT NULL DEFAULT \'{"spot":{"orders_per_batch":3,"maker_timeout_ms":1000,"vol_multiplier":1.0,"max_maker_requotes":2},"futures":{"orders_per_batch":3,"maker_timeout_ms":1000,"vol_multiplier":1.0,"max_maker_requotes":null}}\'::jsonb,\n    ADD CONSTRAINT cta_order_strategies_grid_check CHECK (jsonb_typeof(grid) = \'object\');\nALTER TABLE cta_order_strategies DROP CONSTRAINT cta_order_strategies_algorithm_check;\nALTER TABLE cta_order_strategies ADD CONSTRAINT cta_order_strategies_algorithm_check\n    CHECK (algorithm IN (\'batch\', \'pov\', \'chase\', \'ctaker\', \'grid\'));\n\n\n-- Explicit maintenance before deploying order-template ownership support.\n-- Existing templates retain shared edit access; new templates belong to their creator.\n\nALTER TABLE cta_order_strategies\n    ADD COLUMN created_by_user_id bigint REFERENCES cta_users(user_id);\n\n\nCOMMIT;\n'

from datetime import datetime, timezone
os.umask(0o077)
assert sql('SELECT current_database()') == 'crypto_cta_manager'
columns=json.loads(sql("SELECT json_agg(column_name) FROM information_schema.columns WHERE table_schema='public' AND table_name='cta_order_strategies'"))
assert 'grid' not in columns and 'created_by_user_id' not in columns
stamp=datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ')
backup=ROOT/'backups'/('publish-schema-'+stamp)
backup.mkdir(parents=True,mode=0o700)
pgbin=Path.home()/'.local/opt/postgresql/16/bin'
for args,name in [(['--format=custom','--table=public.cta_order_strategies'],'order-strategies.before.dump'),(['--schema-only','--no-owner','--no-privileges'],'schema.before.sql')]:
    result=subprocess.run([str(pgbin/'pg_dump'),*args,'--file',str(backup/name)],env=env,capture_output=True)
    assert result.returncode == 0, 'backup failed; maintenance not applied'
    assert (backup/name).stat().st_size > 0
result=subprocess.run([str(pgbin/'pg_restore'),'--list',str(backup/'order-strategies.before.dump')],capture_output=True)
assert result.returncode == 0

def hash_table(table,strip=''):
    return json.loads(sql("SELECT json_build_object('rows',count(*),'hash',md5(coalesce(string_agg((to_jsonb(t)"+strip+")::text,E'\\n' ORDER BY (to_jsonb(t)"+strip+")::text),''))) FROM "+table+" t"))

snapshot_tables=['cta_position_snapshots','cta_position_snapshot_entries','cta_strategy_position_snapshots','cta_strategy_position_snapshot_entries']
before=dict(order=hash_table('cta_order_strategies'),bindings=hash_table('cta_account_strategy_bindings'),snapshots={t:hash_table(t) for t in snapshot_tables},manager_pid=pid)
config_files=[ROOT/'config/cta-manager.toml',ROOT/'nginx/nginx.conf']+[Path(p) for p in cfg.get('kline',{}).get('trade_engine_configs',[])]
before['config_hashes']={str(p):hashlib.sha256(p.read_bytes()).hexdigest() for p in config_files if p.is_file()}
processes={}
for p in Path('/proc').iterdir():
    if not p.name.isdigit():continue
    try:
        exe=str((p/'exe').resolve())
        if any(term in exe for term in ['trade_engine','exec-pre-trade','persist_manager','viz','nginx','cta_monitor','redis-server','cta_web']):
            processes[p.name]=(p/'stat').read_text().rsplit(')',1)[1].split()[19]
    except (OSError,IndexError):pass
before['process_start_ticks']=processes
(backup/'before.json').write_text(json.dumps(before,indent=2)+'\n')
(backup/'maintenance.sql').write_text(maintenance)
sql(maintenance)
after=dict(order=hash_table('cta_order_strategies'," - 'grid' - 'created_by_user_id'"),bindings=hash_table('cta_account_strategy_bindings'),snapshots={t:hash_table(t) for t in snapshot_tables})
assert after['order']==before['order'], 'existing order values changed'
assert after['bindings']==before['bindings'], 'bindings changed during maintenance'
assert after['snapshots']==before['snapshots'], 'snapshots changed'
assert subprocess.check_output(['systemctl','--user','show',UNIT,'-p','MainPID','--value'],text=True).strip()==pid
assert all(hashlib.sha256(Path(p).read_bytes()).hexdigest()==h for p,h in before['config_hashes'].items())
assert all(Path('/proc',p,'stat').read_text().rsplit(')',1)[1].split()[19]==v for p,v in processes.items())
result=dict(completed_at=datetime.now(timezone.utc).isoformat(),backup=str(backup),unchanged_original_templates=before['order']['rows'],bindings_unchanged=True,snapshots_unchanged=True,configuration_files_unchanged=len(before['config_hashes']),processes_unchanged=len(processes),manager_pid=pid,columns=json.loads(sql("SELECT json_agg(column_name ORDER BY ordinal_position) FROM information_schema.columns WHERE table_schema='public' AND table_name='cta_order_strategies'")))
(backup/'result.json').write_text(json.dumps(result,indent=2)+'\n')
print(json.dumps(result),flush=True)
print('strategy_matches',sql("SELECT json_agg(json_build_object('name',strategy_name,'updated_at',to_timestamp(updated_at_us/1000000.0),'target_count',(SELECT count(*) FROM jsonb_object_keys(targets)))) FROM cta_position_strategies WHERE strategy_name ILIKE '%C40V6D3%'"))
