import assert from 'node:assert/strict'
import test from 'node:test'
import { applyExecMessage, emptyExecFeed, combinedExecRisk, executionView, execFeedLabel, EXEC_STALE_MS } from '../src/lib/execMonitor.ts'

const now = 1793685600000
const okx = {sourceId:'okex_exec_trade01',venue:'okex-futures',gatewayPrefix:'/exec_okex01'}
const bn = {sourceId:'binance_exec_spread01',venue:'binance-futures',gatewayPrefix:'/exec_binance_spread01'}
const filters = {strategy:'',symbol:'',sourceId:'',groupBy:'symbol'}
function row(overrides={}) {
  return {strategy_name:'spread',symbol:'ETHUSDT',position_allocated:true,execution_complete:false,completion_reason:'',
    source_updated_at_ms:now-1000,account_position_qty:0,account_position_usdt:0,target_qty:1,target_usdt:20,
    current_qty:0,current_usdt:0,effective_position_qty:0,delta_qty:1,delta_usdt:20,live_order_qty:0,live_order_usdt:0,
    pending_qty:1,pending_usdt:20,active_batches:0,remaining_batches:1,estimated_completion_ts_ms:now+10000,...overrides}
}
function state(source,rows,ts=now,ready=true) { return {type:'exec_pre_trade_state',venue:source.venue,entry:{ts_ms:ts,position_ready:ready,rows}} }
function feed(source,rows,ts=now,ready=true) { return applyExecMessage({...emptyExecFeed(source),connection:'live'},state(source,rows,ts,ready)) }
function risk(f,equity,long,short,ts=now) {
  return applyExecMessage(f,{type:'exec_pre_trade_risk',venue:f.source.venue,entry:{ts_ms:ts,venue:f.source.venue,
    equity_usdt:equity,long_notional_usdt:long,short_notional_usdt:short,gross_notional_usdt:long+short,net_notional_usdt:long-short,leverage:999}})
}
test('same symbol and strategy retain exchange ownership; opposite pending orders do not cancel',()=>{
  const feeds=[feed(okx,[row()]),feed(bn,[row({target_qty:-1,target_usdt:-20,delta_qty:-1,delta_usdt:-20,pending_qty:-1,pending_usdt:-20})])]
  const result=executionView(feeds,filters,now)
  assert.equal(result.groups.length,2)
  assert.notEqual(result.rows[0].row.key,result.rows[1].row.key)
  assert.equal(result.summary.deltaUsdt,40)
  assert.equal(result.summary.pendingUsdt,40)
  assert(result.groups.every(g=>g.status.label==='待执行'))
  const strategy=executionView(feeds,{...filters,groupBy:'strategy'},now)
  assert.equal(strategy.groups.length,1)
  assert.equal(strategy.groups[0].target,null)
  assert.equal(strategy.groups[0].current,null)
  assert.equal(strategy.groups[0].pendingUsdt,40)
  assert.equal(strategy.groups[0].status.label,'待执行')
})
test('aggregate equity and exposure are additive; leverage is recomputed from total gross/equity',()=>{
  const feeds=[risk(feed(okx,[]),100,60,0),risk(feed(bn,[]),300,0,140)]
  const sum=combinedExecRisk(feeds,now)
  assert.equal(sum.equity_usdt,400)
  assert.equal(sum.gross_notional_usdt,200)
  assert.equal(sum.net_notional_usdt,-80)
  assert.equal(sum.leverage,0.5)
  assert.equal(combinedExecRisk([risk(feed(okx,[]),0,0,0),risk(feed(bn,[]),0,0,0)],now).leverage,null)
})
test('missing, disconnected and stale legs remain unknown instead of contributing zero',()=>{
  const good=risk(feed(okx,[row()]),100,60,0)
  for (const bad of [emptyExecFeed(bn),{...risk(feed(bn,[row()]),100,60,0),connection:'disconnected'},risk(feed(bn,[row()],now-6000),100,60,0,now-6000)]) {
    assert.equal(combinedExecRisk([good,bad],now).equity_usdt,null)
    assert.equal(executionView([good,bad],filters,now).summary.pendingUsdt,null)
    assert(executionView([good,bad],filters,now).rows.length>=1)
  }
  assert.equal(execFeedLabel(emptyExecFeed(bn),now),'连接中')
  assert.equal(execFeedLabel({...emptyExecFeed(bn),connection:'live'},now),'等待执行数据')
})
test('reloading a cached snapshot cannot refresh old data or overwrite a newer stream message',()=>{
  const original=feed(okx,[row()],now)
  const older=applyExecMessage(original,state(okx,[row({target_qty:999})],now-1000))
  assert.equal(older,original)
  const same=applyExecMessage(original,state(okx,[row({target_qty:999})],now))
  assert.equal(same,original)
  assert.equal(execFeedLabel(same,now+EXEC_STALE_MS+1),'数据延迟')
  const newer=applyExecMessage(same,state(okx,[row({target_qty:2})],now+1000))
  assert.equal(newer.state.rows[0].target_qty,2)
})
test('account position is counted once per venue/symbol, while strategy filtering uses strategy positions',()=>{
  const rows=[row({strategy_name:'a',target_qty:2,target_usdt:40,current_qty:1,current_usdt:20,account_position_qty:3,account_position_usdt:60}),
    row({strategy_name:'b',target_qty:1,target_usdt:20,current_qty:1,current_usdt:20,account_position_qty:3,account_position_usdt:60})]
  const feeds=[feed(okx,rows),feed(bn,[])]
  const all=executionView(feeds,filters,now)
  assert.equal(all.groups[0].current,3)
  assert.equal(all.groups[0].target,3)
  assert.equal(all.summary.deltaUsdt,0)
  const selected=executionView(feeds,{...filters,strategy:'a'},now)
  assert.equal(selected.groups[0].current,1)
  assert.equal(selected.groups[0].target,2)
})
test('unknown or inconsistent repeated account positions do not become a guessed total',()=>{
  const feeds=[feed(okx,[row({strategy_name:'a',account_position_qty:1}),row({strategy_name:'b',account_position_qty:2,account_position_usdt:null})]),feed(bn,[])]
  const result=executionView(feeds,filters,now)
  assert.equal(result.groups[0].current,null)
  assert.equal(result.summary.deltaUsdt,null)
})
test('exchange-minimum completed residual keeps its factual delta but no executable pending amount',()=>{
  const feeds=[feed(okx,[row({execution_complete:true,completion_reason:'exchange_minimum',remaining_batches:0})]),feed(bn,[])]
  const result=executionView(feeds,filters,now)
  assert.equal(result.summary.pendingUsdt,0)
  assert.equal(result.summary.deltaUsdt,20)
  assert.equal(result.rows[0].status.label,'执行完成')
  assert.equal(result.rows[0].status.detail,'剩余量低于交易所最小下单量')
})
test('working execution remains working even when signed deltas or live orders offset',()=>{
  const feeds=[feed(okx,[row({live_order_qty:1,live_order_usdt:20,active_batches:1})]),
    feed(bn,[row({delta_usdt:-20,live_order_qty:-1,live_order_usdt:-20,active_batches:1})])]
  const result=executionView(feeds,{...filters,groupBy:'strategy'},now)
  assert.equal(result.groups[0].status.label,'执行中')
  assert.equal(result.summary.liveUsdt,40)
})
test('no snapshot and an unready empty snapshot differ from a fresh, ready empty account',()=>{
  assert.equal(executionView([emptyExecFeed(okx),emptyExecFeed(bn)],filters,now).summary.deltaUsdt,null)
  assert.equal(executionView([feed(okx,[],now,false),feed(bn,[])],filters,now).summary.deltaUsdt,null)
  assert.equal(executionView([feed(okx,[]),feed(bn,[])],filters,now).summary.deltaUsdt,0)
  const unallocated=executionView([feed(okx,[row({position_allocated:false})]),feed(bn,[])],filters,now)
  assert.equal(unallocated.groups[0].current,null)
  assert.equal(unallocated.summary.pendingUsdt,null)
})
test('shared strategy, symbol and venue filters compose across both feeds',()=>{
  const feeds=[feed(okx,[row(),row({symbol:'BTCUSDT'}),row({symbol:'BTC',strategy_name:'cash'})]),feed(bn,[row(),row({symbol:'SOLUSDT',strategy_name:'other'})])]
  assert.deepEqual(executionView(feeds,filters,now).strategies,['cash','other','spread'])
  assert.equal(executionView(feeds,{...filters,symbol:'eth',strategy:'spread'},now).groups.length,2)
  assert.equal(executionView(feeds,{...filters,symbol:'eth',sourceId:bn.sourceId},now).groups.length,1)
  const btc=executionView(feeds,{...filters,symbol:'BTC'},now)
  assert.equal(btc.groups.length,2)
  assert.notEqual(btc.groups[0].venue,btc.groups[1].venue)
})
test('venue mismatch, coin-margined symbols, malformed rows and duplicate identities are rejected',()=>{
  const f=emptyExecFeed(okx)
  for(const value of [state(bn,[row()]),state(okx,[row({symbol:'BTCUSD'})]),state(okx,[row({target_qty:'1'})]),state(okx,[row(),row()]),state(okx,[row({symbol:null})])])assert.throws(()=>applyExecMessage(f,value))
  assert.throws(()=>applyExecMessage(emptyExecFeed(bn),state(bn,[row({symbol:'BTC'})])))
  assert.equal(applyExecMessage(f,{type:'unrelated',entry:{}}),f)
})
