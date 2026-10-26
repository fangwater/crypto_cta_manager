import assert from 'node:assert/strict'
import test from 'node:test'
import { overviewTotal, overviewAlertCount } from '../src/lib/accountOverview.ts'

test('filtered totals preserve losses and distinguish missing data from zero', () => {
  const rows = [{ equity: 100, today_profit: 5 }, { equity: 200, today_profit: -9 }]
  assert.equal(overviewTotal(rows, 'equity'), 300)
  assert.equal(overviewTotal(rows, 'today_profit'), -4)
  assert.equal(overviewTotal(rows.slice(1), 'today_profit'), -9)
  assert.equal(overviewTotal([{ equity: 0 }], 'equity'), 0)
  assert.equal(overviewTotal([{ equity: 100 }, { equity: null }], 'equity'), null)
  assert.equal(overviewTotal([{ equity: NaN }], 'equity'), null)
})

test('unknown alert configuration or input does not mean zero alerts', () => {
  assert.equal(overviewAlertCount([{ leverage_alert: null }], 'leverage_alert'), null)
  assert.equal(overviewAlertCount([{ timeout_alert: false }, { timeout_alert: true }], 'timeout_alert'), 1)
  assert.equal(overviewAlertCount([{ timeout_alert: false }], 'timeout_alert'), 0)
})

test('FR aggregation preserves losses and suppresses stale or unavailable totals', () => {
  const cta = { equity: 100, today_profit: 5 }
  const fr = { account_kind: 'fr', collection_status: 'live', equity: 200, today_profit: -9 }
  assert.equal(overviewTotal([cta, fr], 'equity'), 300)
  assert.equal(overviewTotal([cta, fr], 'today_profit'), -4)
  assert.equal(overviewTotal([cta, { ...fr, collection_status: 'historical' }], 'equity'), 300)
  for (const collection_status of ['stale', 'loading', 'unavailable']) {
    assert.equal(overviewTotal([cta, { ...fr, collection_status }], 'equity'), null)
    assert.equal(overviewTotal([cta], 'equity'), 100)
  }
})
