"""Run over SSH on el01; print only sanitized funding deployment evidence."""
import base64
import math
import datetime
import hashlib
import http.client
import json
import os
import pathlib
import socket
import subprocess
import time
import tomllib
import urllib.parse

root = pathlib.Path('/home/el01/crypto_cta_manager')
config = tomllib.loads((root / 'config/cta-manager.toml').read_text())
source_id = 'binance_exec_trade09'
values = {}
for line in pathlib.Path('/home/el01/.config/crypto-cta-manager/database.env').read_text().splitlines():
    line = line.strip().removeprefix('export ').strip()
    if line and not line.startswith('#') and '=' in line:
        name, value = line.split('=', 1)
        values[name] = value.strip().strip('"').strip("'")
url = urllib.parse.urlsplit(values[config['database']['url_env']])
db_env = os.environ.copy()
db_env.update(PGHOST=url.hostname, PGPORT=str(url.port),
              PGUSER=urllib.parse.unquote(url.username),
              PGPASSWORD=urllib.parse.unquote(url.password),
              PGDATABASE=url.path.lstrip('/'), PGCONNECT_TIMEOUT='5')


def emit(kind, **fields):
    print(json.dumps({'kind': kind, **fields}, ensure_ascii=False), flush=True)


def sql(query):
    result = subprocess.run(['/home/el01/.local/opt/postgresql/16/bin/psql',
                             '-X', '-A', '-t', '-v', 'ON_ERROR_STOP=1'],
                            input=query, env=db_env, capture_output=True, text=True)
    if result.returncode:
        raise RuntimeError('database verification failed')
    return result.stdout.strip()


def request(path, token=None, method='GET', body=None):
    connection = http.client.HTTPConnection('127.0.0.1', 18201, timeout=90)
    try:
        headers = {'Cookie': 'cta_session=' + token} if token else {}
        if body is not None:
            headers['Content-Type'] = 'application/json'
        connection.request(method, path, body=json.dumps(body) if body is not None else None, headers=headers)
        response = connection.getresponse()
        return response.status, json.loads(response.read())
    finally:
        connection.close()


def get(path, token=None):
    return request(path, token)


def funding_state():
    return json.loads(sql("SELECT json_build_object("
                          "'accounts',(SELECT count(*) FROM cta_funding_accounts),"
                          "'symbols',(SELECT count(*) FROM cta_funding_symbols WHERE source_id='binance_exec_trade09'),"
                          "'synced_symbols',(SELECT count(*) FROM cta_funding_symbols WHERE source_id='binance_exec_trade09' AND synced_through_ts_us IS NOT NULL),"
                          "'records',(SELECT count(*) FROM cta_funding_income WHERE source_id='binance_exec_trade09'),"
                          "'observation_start_us',(SELECT start_ts_us FROM cta_funding_accounts WHERE source_id='binance_exec_trade09'),"
                          "'synced_through_us',(SELECT min(synced_through_ts_us) FROM cta_funding_symbols WHERE source_id='binance_exec_trade09'));"))


def https_bindings():
    pid = subprocess.check_output(['systemctl', '--user', 'show',
                                   'crypto-cta-manager-web.service',
                                   '--property=MainPID', '--value'], text=True).strip()
    proc = pathlib.Path('/proc') / pid
    inodes = set()
    for fd in (proc / 'fd').iterdir():
        try:
            name = os.readlink(fd)
            if name.startswith('socket:['):
                inodes.add(name[8:-1])
        except OSError:
            pass
    addresses = set()
    for line in (proc / 'net/tcp').read_text().splitlines()[1:]:
        fields = line.split()
        if fields[9] in inodes and int(fields[2].split(':')[1], 16) == 443:
            addresses.add(socket.inet_ntoa(bytes.fromhex(fields[1].split(':')[0])[::-1]))
    return sorted(addresses)


import decimal
import hmac
source=next(s for s in config['sources'] if s['id']==source_id)
settings=json.loads(sql("SELECT json_build_object('enabled',funding_enabled,'verified_at',funding_verified_at_us,'ip',funding_verified_public_ip) FROM cta_order_sources WHERE source_id='binance_exec_trade09';"))
assert settings['enabled'] and settings['ip']=='154.197.32.9'
start=int(sql("SELECT start_ts_us FROM cta_funding_symbols WHERE source_id='binance_exec_trade09' AND symbol='BTCUSDT';"));start=((start+999)//1000)*1000;end=start+7*86400000000-1000
credentials={}
for line in pathlib.Path('/home/el01/binance_exec_trade09/env.sh').read_text().splitlines():
    line=line.strip().removeprefix('export ').strip()
    if line and not line.startswith('#') and '=' in line:
        name,value=line.split('=',1);credentials[name.strip()]=value.strip().strip('"').strip("'")
assert credentials.get('BINANCE_ACCOUNT_MODE','STANDARD').upper()=='STANDARD'
params={'symbol':'BTCUSDT','incomeType':'FUNDING_FEE','startTime':start//1000,'endTime':end//1000,'limit':1000,'page':1,'recvWindow':5000,'timestamp':int(time.time()*1000)}
query=urllib.parse.urlencode(sorted(params.items()))
signature=hmac.new(credentials['BINANCE_API_SECRET'].encode(),query.encode(),hashlib.sha256).hexdigest()
base=urllib.parse.urlsplit(credentials.get('BINANCE_FAPI_URL','https://fapi.binance.com'))
assert base.scheme=='https'
connection=http.client.HTTPSConnection(base.hostname,base.port or 443,timeout=15,source_address=('154.197.32.9',0))
connection.request('GET','/fapi/v1/income?'+query+'&signature='+signature,headers={'X-MBX-APIKEY':credentials['BINANCE_API_KEY']})
response=connection.getresponse();payload=response.read();connection.close()
assert response.status==200,'independent bound income check failed'
rows=json.loads(payload);assert isinstance(rows,list) and len(rows)<1000
exchange={str(row['tranId']):(decimal.Decimal(row['income']),row['time']*1000) for row in rows}
assert len(exchange)==len(rows) and all(row['incomeType']=='FUNDING_FEE' and row['symbol']=='BTCUSDT' for row in rows)
ledger=json.loads(sql("SELECT coalesce(json_agg(json_build_object('id',tran_id,'income',income::text,'ts',ts_us)),'[]') FROM cta_funding_income WHERE source_id='binance_exec_trade09' AND symbol='BTCUSDT' AND ts_us BETWEEN "+str(start)+" AND "+str(end)+";"))
persisted={row['id']:(decimal.Decimal(row['income']),row['ts']) for row in ledger}
assert persisted==exchange,'persisted funding differs from Binance income'
emit('independent_income_check',source_id=source_id,symbol='BTCUSDT',local_ip='154.197.32.9',http=response.status,start_ts_us=start,end_ts_us=end,records=len(rows),income_decimal=str(sum((value[0] for value in exchange.values()),decimal.Decimal(0))),ledger_matches_exchange=True)
