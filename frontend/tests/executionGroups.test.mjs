import assert from 'node:assert/strict'
import test from 'node:test'
import {executionGroups, groupAccount, accountExecutionGroup, estimatedFundingOnly, requireGroupSources} from '../src/lib/executionGroups.ts'
import {routes} from '../src/lib/routes.ts'
const group=executionGroups[0]
const okx={source_id:group.sourceIds[0],account:'okex_exec_cross01',venue:'okex-futures',enabled:true,gateway_prefix:'/exec_okex01'}
const bn={source_id:group.sourceIds[1],account:'binance_exec_cross01',venue:'binance-futures',enabled:true,gateway_prefix:'/exec_binance_spread01'}
const account={source_id:group.accountId,account:'okex_binace_cross_1min',venue:'spread',enabled:true,gateway_prefix:null,execution_sources:[bn,okx]}
test('one public account owns both execution panes without exposing two account entries',()=>{
 assert.deepEqual(groupAccount([account],group),{account,executions:[okx,bn]})
 assert.equal(groupAccount([bn,okx],group),null)
 assert.equal(routes.execViz(accountExecutionGroup(account).id),'/manager/exec/?group=cross01')
 assert.equal(accountExecutionGroup(okx),null)
 assert.equal(estimatedFundingOnly(account.source_id),true)
 assert.equal(estimatedFundingOnly(okx.source_id),true)
 assert.equal(estimatedFundingOnly(bn.source_id),true)
 assert.equal(estimatedFundingOnly('unrelated-account'),false)
})
test('no frame is exposed with a hidden account or incomplete execution permissions',()=>{
 assert.equal(groupAccount([],group),null)
 assert.equal(groupAccount([{...account,enabled:false}],group),null)
 for(const executions of [[okx],[bn],[okx,okx],[okx,bn,bn],[okx,{...bn,enabled:false}],[okx,{...bn,gateway_prefix:null}],[okx,{...bn,execution_sources:[{}]}]]) {
  const incomplete={...account,execution_sources:executions}
  assert.equal(groupAccount([incomplete],group),null)
  assert.equal(accountExecutionGroup(incomplete),null)
 }
})
test('iframe URLs come only from valid same-origin configured gateway prefixes',()=>{
 for(const gateway_prefix of ['https://example.com','//example.com','/exec/a','/exec?x=1','/exec#part'])assert.equal(groupAccount([{...account,execution_sources:[okx,{...bn,gateway_prefix}]}],group),null)
 assert.equal(groupAccount([{...account,execution_sources:[okx,{...bn,venue:'binance-coin-futures'}]}],group),null)
})
test('combined PnL never accepts a partial, duplicate or broader account response',()=>{
 requireGroupSources([...group.sourceIds].reverse(),group.sourceIds)
 for(const ids of [[],[okx.source_id],[okx.source_id,okx.source_id],[...group.sourceIds,'other'],[okx.source_id,'other']])assert.throws(()=>requireGroupSources(ids,group.sourceIds))
})
