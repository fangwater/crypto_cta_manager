source = 'binance_exec_trade03'
with gateway() as api:
    old = api('catalog/accounts/'+source)
    save('trade03.fees.before.json', old)
    rates = dict(maker_fee_rate=0.000144, taker_fee_rate=0.00036,
                 theoretical_twap_fee_rate=0.000252)
    emit('fee_update', rates)
    result = api('catalog/accounts/'+source+'/fee-rates', rates, 'PUT')
    if 'http' in result:
        emit('fee_update_response', result)
    after = api('catalog/accounts/'+source)
    save('trade03.fees.after.json', after)
    verified = json.loads(sql("SELECT row_to_json(t) FROM (SELECT source_id,maker_fee_rate,taker_fee_rate,theoretical_twap_fee_rate,quote_fee_rates FROM cta_order_sources WHERE source_id='binance_exec_trade03') t;"))
    assert all(verified[k] == v for k,v in rates.items())
    emit('fee_rates_verified', verified)
