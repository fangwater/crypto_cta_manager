phase=sys.argv[1]
if phase=='before':
    end=(int(time.time())//60-30)*60000
    window=dict(startMs=end-86400000,endMs=end,maxPoints=60,includeTheoretical='false')
    (WORK/'nav_window.json').write_text(json.dumps(window)+'\n')
else:
    window=json.loads((WORK/'nav_window.json').read_text())
captured={}
with gateway() as get:
    for source in ['binance_exec_trade04','binance_exec_trade09']:
        started=time.monotonic()
        while True:
            try:
                body=get('account-timeline?'+urllib.parse.urlencode(dict(window,sourceIds=source)))
                break
            except urllib.error.HTTPError as error:
                if error.code!=503:raise
                reason=json.loads(error.read())
                emit('nav_pending',dict(source=source,**reason))
                assert reason.get('loading') and time.monotonic()-started<600
                time.sleep(10)
        captured[source]={key:body['report'][key] for key in ['valuation','summary','symbols','points']}
        emit('nav',dict(source=source,report_sha256=digest(captured[source]),summary=body['report']['summary']))
(WORK/('nav_'+phase+'.json')).write_text(json.dumps(captured,indent=2)+'\n')
if phase=='after':
    before=json.loads((WORK/'nav_before.json').read_text())
    differences={source:digest(row)!=digest(before[source]) for source,row in captured.items()}
    emit('nav_comparison',dict(window=window,changed=differences))
    assert not any(differences.values())
