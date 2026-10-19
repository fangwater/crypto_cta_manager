import assert from 'node:assert/strict'
import test from 'node:test'
import { accountMetrics, workspaceAccounts } from '../src/lib/workspace.ts'

const account = { source_id: 'one', venue: 'binance-futures', live_equity_usdt: 100, live_equity_status: 'ok' }
const report = { nav_change_after_fee_quote: -3, symbols: [{ long_position_value_quote: 200, short_position_value_quote: -50 }, { long_position_value_quote: 20, short_position_value_quote: -10 }] }

test('account exposure preserves short magnitude and net direction', () => {
  assert.deepEqual(accountMetrics(account, report), { equity: 100, long: 220, short: 60, net: 160, longLeverage: 2.2, shortLeverage: 0.6, pnl: -3 })
})

test('missing or stale equity, nonpositive equity and unlike currencies cannot produce leverage', () => {
  for (const change of [{ live_equity_status: 'stale' }, { live_equity_status: null }, { live_equity_usdt: 0 }, { live_equity_usdt: -1 }, { live_equity_usdt: null }, { venue: 'binance-coin-futures' }]) {
    const metrics = accountMetrics({ ...account, ...change }, report)
    assert.equal(metrics.longLeverage, null)
    assert.equal(metrics.shortLeverage, null)
  }
  assert.equal(accountMetrics(account).long, null)
  assert.equal(accountMetrics(account, { ...report, symbols: [] }).long, 0)
})

test('an empty visible catalog must not expose fallback source accounts', () => {
  assert.deepEqual(workspaceAccounts({ accounts: [], report: { sources: [{ source_id: 'hidden' }] } }), [])
})
