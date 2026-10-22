
try:
 data=json.loads((OUT/'after.json').read_text())
 for source in ['binance_exec_trade06','binance_exec_trade07']:
  params=dict(startMs=window['endMs']-3600000,endMs=window['endMs'],sourceIds=source,pageSize=100)
  began=time.monotonic()
  while True:
   body=get('/manager/api/catalog/acquisition-cost?'+urllib.parse.urlencode(params))
   previous=data['queries'][source+'_cost']
   for i, report in enumerate(body['reports']):
    if not any('目标历史' in warning for warning in report['warnings']) and report['totals']['missing_virtual_delta_count']==0 and report['totals']['pending_virtual_delta_count']==0:
     previous['reports'][i]=report
   (OUT/'after.json').write_text(json.dumps(data))
   if not any(any('目标历史' in warning for warning in r['warnings']) for r in previous['reports']):
    body=previous
    break
   emit('waiting',{'source':source,'warnings':[r['warnings'] for r in body['reports']]})
   assert time.monotonic()-began<600
   time.sleep(5)
  data['queries'][source+'_cost']=body
  emit('cost_ready',{'source':source,'rows':[len(r['rows']) for r in body['reports']]})
 (OUT/'after.json').write_text(json.dumps(data))
 emit('cache',get('/manager/api/catalog/kline-status'))
finally:
 sql('DELETE FROM cta_auth_sessions WHERE session_id='+session_id+';',readonly=False)
 emit('diagnostic_session_removed',True)
