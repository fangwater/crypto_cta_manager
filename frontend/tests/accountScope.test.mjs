import assert from 'node:assert/strict'
import test from 'node:test'
import { accountScope } from '../src/lib/accountScope.ts'
import { routes } from '../src/lib/routes.ts'

const okx = { source_id: 'okx', account: 'OKX', venue: 'okex-futures' }
const bn = { source_id: 'bn', account: 'Binance', venue: 'binance-futures' }
const spread = { source_id: 'spread', account: 'okex_binace_cross_1min', execution_sources: [okx, bn] }
const other = { source_id: 'other', account: 'other' }
const accounts = [spread, other]

test('one account retains its public identity when opening either execution configuration', () => {
  const overview = accountScope(accounts, 'spread')
  assert.equal(overview.account, spread)
  assert.equal(overview.execution, null)
  for (const execution of [okx, bn]) {
    const scope = accountScope(accounts, 'spread', execution.source_id)
    assert.equal(scope.account, spread)
    assert.equal(scope.execution, execution)
    for (const path of [routes.account(scope.account.source_id), routes.configBindings(scope.account.source_id)]) {
      const url = new URL(path, 'http://manager')
      assert.equal(url.searchParams.get('source'), 'spread')
      assert.equal(url.searchParams.has('executionSource'), false)
    }
  }
})

test('a leg from another account never becomes a configuration target', () => {
  assert.throws(() => accountScope(accounts, 'spread', 'other'), /执行腿不属于当前账户/)
  assert.throws(() => accountScope(accounts, 'other', 'okx'), /执行腿不属于当前账户/)
  assert.equal(accountScope(accounts, 'hidden'), null)
  assert.equal(accountScope(accounts, 'okx').account, spread)
  assert.equal(accountScope(accounts, 'okx').execution, okx)
})

test('native accounts keep their ordinary URL and configuration scope', () => {
  assert.equal(accountScope(accounts, 'other').execution, other)
  assert.equal(routes.account('other'), '/manager/account/?source=other')
  assert.equal(routes.configBindings('other'), '/manager/config/bindings/?source=other')
})
