"""Read-only RapidX statement and bound public-IP probes for LGQ's three sources."""
import hashlib
import hmac
import http.client
import json
import pathlib
import time
import tomllib
import urllib.parse

out=[]
for number in ['05','06','07']:
    root=pathlib.Path('/home/ubuntu/binance_exec_trade'+number)
    ips=tomllib.loads((root/'trade_engine.toml').read_text())['local_ips']
    assert ips and ips[0] not in ['0.0.0.0','::'], 'probe requires explicit current binding'
    ip=ips[0]
    matches=[]
    for p in pathlib.Path('/proc').iterdir():
        if not p.name.isdecimal(): continue
        try:
            if (p/'comm').read_text().strip()=='trade_engine' and (p/'cwd').resolve()==root:
                matches.append(p)
        except OSError: pass
    assert len(matches)==1
    wanted={b'LTP_API_KEY',b'LTP_API_SECRET',b'LTP_PORTFOLIO_ID',b'LTP_REST_URL'}
    env={k.decode():v.decode() for item in (matches[0]/'environ').read_bytes().split(b'\0') if b'=' in item for k,v in [item.split(b'=',1)] if k in wanted}
    assert all(env.get(x) for x in ['LTP_API_KEY','LTP_API_SECRET','LTP_PORTFOLIO_ID'])
    assert env.get('LTP_REST_URL','https://api.liquiditytech.com').rstrip('/')=='https://api.liquiditytech.com'
    conn=http.client.HTTPSConnection('api.ipify.org',timeout=15,source_address=(ip,0))
    conn.request('GET','/')
    response=conn.getresponse(); assert response.status==200
    public=response.read().decode().strip(); conn.close()
    end=time.time_ns()//1_000_000
    params={'exchange':'BINANCE','startTime':str(end-86400000),'endTime':str(end),'statementType':'FUNDING_FEE','page':'1','pageSize':'1000'}
    nonce=str(time.time_ns()//1_000_000_000)
    signed='&'.join(k+'='+params[k] for k in sorted(params))+'&'+nonce
    headers={'X-MBX-APIKEY':env['LTP_API_KEY'],'nonce':nonce,'ts':str(time.time_ns()//1000),
             'signature':hmac.new(env['LTP_API_SECRET'].encode(),signed.encode(),hashlib.sha256).hexdigest()}
    conn=http.client.HTTPSConnection('api.liquiditytech.com',timeout=15,source_address=(ip,0))
    conn.request('GET','/api/v1/trading/statement?'+urllib.parse.urlencode(sorted(params.items())),headers=headers)
    response=conn.getresponse(); body=json.loads(response.read()); conn.close()
    assert response.status==200 and body.get('code') in [200,200000], ('statement failed',root.name,response.status)
    data=body['data']; rows=data['list']
    assert all(str(r['portfolioId'])==env['LTP_PORTFOLIO_ID'] and r['exchangeType']=='BINANCE' and r['statementType']=='FUNDING_FEE' for r in rows)
    out.append({'source_id':root.name,'local_ip':ip,'public_ip':public,'http_status':response.status,
                'total':data['totalSize'],'empty_symbols':sum(not r['sym'] for r in rows),
                'sample':[{k:r.get(k) for k in ['statementId','coin','sym','statementType','businessType','deltaAmount','createAt']} for r in rows[:3]]})
path=pathlib.Path('/home/ubuntu/crypto_cta_manager/backups/rx01_initial_snapshot_20261015/funding_probe.json')
path.write_text(json.dumps(out,indent=2)+'\n')
print(json.dumps(out,indent=2))
