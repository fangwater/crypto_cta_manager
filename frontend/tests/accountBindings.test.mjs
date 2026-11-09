import assert from 'node:assert/strict'
import test from 'node:test'
import { accountBindings } from '../src/lib/accountBindings.ts'
const binding = (source_id, overrides = {}) => ({source_id, binding_name:'spread', position_strategy_name:'spread-pos', order_strategy_name:'maker', shares:1, updated_at_us:1, ...overrides})
const studio = (...bindings) => ({bindings})

test('same strategy in two markets is one account strategy; shares are never summed', () => {
  const rows = accountBindings([studio(binding('okx')), studio(binding('bn'))])
  assert.equal(rows.length, 1)
  assert.equal(rows[0].shares, 1)
  assert.equal(rows[0].markets.length, 2)
  assert.deepEqual(rows[0].markets.map(b => b.source_id), ['okx', 'bn'])
})
test('different market parameters stay explicit inside the same account strategy', () => {
  const rows = accountBindings([studio(binding('okx')), studio(binding('bn', {shares:2,order_strategy_name:'taker'}))])
  assert.equal(rows.length, 1)
  assert.equal(rows[0].shares, null)
  assert.equal(rows[0].order_strategy_name, '按市场配置')
  assert.deepEqual(rows[0].markets.map(b => [b.order_strategy_name, b.shares]), [['maker',1],['taker',2]])
})
test('distinct strategies and absent market bindings retain their factual scope', () => {
  const rows = accountBindings([studio(binding('okx')), studio(binding('bn',{binding_name:'another',position_strategy_name:'another-pos'}))])
  assert.equal(rows.length, 2)
  assert(rows.every(b => b.markets.length === 1))
  assert.equal(accountBindings([]).length, 0)
})
