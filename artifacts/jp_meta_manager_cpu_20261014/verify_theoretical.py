"""Probe authenticated read-only APIs through Nginx; remove the test session."""
import hashlib
import json
import math
import os
from pathlib import Path
import secrets
import subprocess
import time
import tomllib
import urllib.error
import urllib.parse
import urllib.request

ROOT = Path('/home/ubuntu/crypto_cta_manager')
STAGE = ROOT / 'deploy-staging/manager-cpu24-20261014'
UNIT = 'crypto-cta-manager-web.service'
cfg = tomllib.loads((ROOT / 'config/cta-manager.toml').read_text())
pid = subprocess.check_output(['systemctl', '--user', 'show', UNIT, '-p', 'MainPID', '--value'], text=True).strip()
runtime = dict(entry.split(b'=', 1) for entry in Path(f'/proc/{pid}/environ').read_bytes().split(b'\0') if b'=' in entry)
url = urllib.parse.urlsplit(runtime[cfg.get('database', {}).get('url_env', 'CRYPTO_CTA_LOCAL_DATABASE_URL').encode()].decode())
env = dict(os.environ, PGHOST=url.hostname, PGPORT=str(url.port or 5432),
           PGUSER=urllib.parse.unquote(url.username), PGPASSWORD=urllib.parse.unquote(url.password or ''),
           PGDATABASE=urllib.parse.unquote(url.path.lstrip('/')), PGCONNECT_TIMEOUT='10')


def sql(query):
    result = subprocess.run(['psql', '-X', '-q', '-A', '-t', '-v', 'ON_ERROR_STOP=1'],
                            input=query, text=True, capture_output=True, env=env)
    if result.returncode:
        raise RuntimeError('verification database operation failed (credentials suppressed)')
    return result.stdout.strip()


token = secrets.token_urlsafe(32)
digest = hashlib.sha256(token.encode()).hexdigest()
session_id = sql("INSERT INTO cta_auth_sessions (user_id,token_hash,expires_at) SELECT user_id,decode('" + digest +
                 "','hex'),now()+interval '45 minutes' FROM cta_users WHERE role='admin' AND disabled=false ORDER BY user_id LIMIT 1 RETURNING session_id;")
assert session_id.isdigit()
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def get(path):
    began = time.monotonic()
    request = urllib.request.Request('http://127.0.0.1:4191/manager/api/' + path,
                                     headers={'Cookie': 'cta_session=' + token})
    with opener.open(request, timeout=175) as response:
        result = json.load(response)
    return result, round(time.monotonic() - began, 3)


def emit(value):
    print(json.dumps(value, ensure_ascii=False), flush=True)


try:
    observations = []
    for attempt in range(120):
        status, elapsed = get('catalog/kline-status')
        target = status['target_history']
        observation = {'time_us': time.time_ns() // 1000, 'target_history': target, 'elapsed': elapsed}
        observations.append(observation)
        (STAGE / 'target_observations.json').write_text(json.dumps(observations, indent=2))
        emit(observation)
        assert target['last_error'] is None, target
        if target['ready']:
            break
        time.sleep(15)
    else:
        raise RuntimeError('target history still loading after 30 minutes')
    # Freeze each query across automatic retries so the result really completes.
    end_ms = int(time.time() * 1000)
    queries = {}
    for source in cfg['sources']:
        if source.get('enabled', True):
            source_id = source['id']
            queries[source_id] = {'sourceIds': source_id,
                                  'startMs': end_ms - (3600000 if source_id.endswith('06') else 86400000),
                                  'endMs': end_ms, 'includeTheoretical': 'true', 'maxPoints': 1500}
    summaries = {}
    for attempt in range(30):
        pending = []
        for source_id, query in queries.items():
            if source_id in summaries and summaries[source_id]['points']:
                continue
            try:
                result, elapsed = get('timeline?' + urllib.parse.urlencode(query))
            except urllib.error.HTTPError as error:
                emit({'source': source_id, 'http_error': error.code, 'body': error.read().decode()})
                raise
            theoretical = result['theoretical']
            summary = {'source': source_id, 'elapsed': elapsed, 'points': len(theoretical['points']),
                       'factual_points': len(result['report']['points']), 'loading': theoretical['loading'],
                       'missing': theoretical['missing_price_count'], 'reason': theoretical['unavailable_reason'],
                       'latest_point_ts_us': theoretical['latest_point_ts_us']}
            emit(summary)
            summaries[source_id] = summary
            (STAGE / ('timeline_' + source_id + '.json')).write_text(json.dumps(result))
            if theoretical['loading']:
                pending.append(source_id)
            else:
                assert theoretical['points'], summary
                assert theoretical['missing_price_count'] == 0 and theoretical['unavailable_reason'] is None, summary
                assert all(all(math.isfinite(point[key]) for key in ['nav_change_before_fee_quote', 'nav_change_after_fee_quote', 'estimated_trading_fee_quote'])
                           and abs(point['nav_change_before_fee_quote'] - point['nav_change_after_fee_quote'] - point['estimated_trading_fee_quote']) < 1e-7
                           for point in theoretical['points'])
        (STAGE / 'timeline_summaries.json').write_text(json.dumps(summaries, indent=2))
        if not pending:
            break
        time.sleep(15)
    else:
        raise RuntimeError('theoretical backfill did not complete')
    repeats = []
    for source_id, query in queries.items():
        before, _ = get('catalog/kline-status')
        result, elapsed = get('timeline?' + urllib.parse.urlencode(query))
        after, _ = get('catalog/kline-status')
        old = json.loads((STAGE / ('timeline_' + source_id + '.json')).read_text())
        assert result['report'] == old['report'], ('factual repeat changed', source_id)
        assert result['theoretical']['points'] == old['theoretical']['points'], ('theoretical repeat changed', source_id)
        repeat = {'source': source_id, 'elapsed': elapsed,
                  'identical_points': True, 'request_delta': after['requests'] - before['requests']}
        repeats.append(repeat)
        emit(repeat)
    (STAGE / 'repeat_summaries.json').write_text(json.dumps(repeats, indent=2))
    health, _ = get('health')
    status, _ = get('catalog/kline-status')
    (STAGE / 'final_health.json').write_text(json.dumps(health, indent=2))
    (STAGE / 'final_kline_status.json').write_text(json.dumps(status, indent=2))
    assert health['status'] == 'ok' and health['last_refresh_error'] is None
    emit({'verified': True, 'health': health, 'target_history': status['target_history']})
finally:
    sql('DELETE FROM cta_auth_sessions WHERE session_id=' + session_id + ';')
    emit({'verification_session_removed': True})
