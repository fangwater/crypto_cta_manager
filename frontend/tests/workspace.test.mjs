import assert from 'node:assert/strict'
import test from 'node:test'
import { overviewTotal, overviewAlertCount, isExternalOverview, nativeSourceStatus } from '../src/lib/accountOverview.ts'
import { isNativeFuturesVenue, liveAccountEquity, moneyWithUnit, quoteUnit } from '../src/format.ts'

test('dashboard equity uses the field and currency for the account market', () => {
  for (const venue of ['binance-futures', 'binance-coin-futures', 'okex-futures', 'spread']) {
    assert.equal(quoteUnit(isNativeFuturesVenue(venue)), '报价币')
    assert.equal(moneyWithUnit(liveAccountEquity({ venue, live_equity_usdt: 125.5 }), 'USDT'), '125.50 USDT')
    assert.equal(liveAccountEquity({ venue, live_equity_usdt: null, live_equity_quote: 500 }), null)
  }
  for (const venue of ['ctp', 'yida']) {
    const currency = quoteUnit(isNativeFuturesVenue(venue))
    assert.equal(moneyWithUnit(liveAccountEquity({ venue, live_equity_quote: 0 }), currency), '0.00 CNY')
    assert.equal(moneyWithUnit(liveAccountEquity({ venue, live_equity_usdt: 500 }), currency), '--')
  }
})

test('native sync and execution statuses remain distinct', () => {
  assert.equal(nativeSourceStatus({ collection_status: 'live', execution_status: 'quiet' }).label, '已同步')
  assert.deepEqual(nativeSourceStatus({ collection_status: 'live', execution_status: 'stopped' }), {
    label: '已停止', tone: 'neutral', title: '已同步',
  })
  assert.equal(nativeSourceStatus({ collection_status: 'live', execution_status: 'recovering' }).label, '对账中')
  assert.equal(nativeSourceStatus({ collection_status: 'loading' }).label, '同步中')
  assert.equal(nativeSourceStatus({ collection_status: 'stale' }).label, '同步延迟')
})

test('MM and remote CTA use external status while local CTA keeps its local rules', () => {
  const mm = { source_id: 'external:nav:bybit_mm_alpha', account_kind: 'mm', equity: 100, collection_status: 'live' }
  const exec = { source_id: 'external:sg:bybit_exec_trade01', account_kind: 'cta', equity: 200, collection_status: 'live' }
  assert.equal(isExternalOverview(mm), true)
  assert.equal(isExternalOverview(exec), true)
  assert.equal(overviewTotal([mm, exec], 'equity'), 300)
  assert.equal(overviewTotal([mm, { ...exec, collection_status: 'stale' }], 'equity'), null)
  assert.equal(isExternalOverview({ source_id: 'bybit_exec_trade01', account_kind: 'cta' }), false)
})

test('filtered totals preserve losses and distinguish missing data from zero', () => {
  const rows = [{ equity: 100, today_profit: 5 }, { equity: 200, today_profit: -9 }]
  assert.equal(overviewTotal(rows, 'equity'), 300)
  assert.equal(overviewTotal(rows, 'today_profit'), -4)
  assert.equal(overviewTotal(rows.slice(1), 'today_profit'), -9)
  assert.equal(overviewTotal([{ equity: 0 }], 'equity'), 0)
  assert.equal(overviewTotal([{ equity: 100 }, { equity: null }], 'equity'), null)
  assert.equal(overviewTotal([{ equity: NaN }], 'equity'), null)
})

test('previous native fund observations remain visible but cannot enter a current total', () => {
  const live = { source_id: 'ctp', quote_currency: 'CNY', funds_status: 'live', equity: 100 }
  const zero = { source_id: 'ctp-zero', quote_currency: 'CNY', funds_status: 'live', equity: 0 }
  const old = { ...live, funds_status: 'historical', equity: 200 }
  const usableOnly = { source_id: 'yida', funds_status: 'live', equity: null, usable_cash: 400, sdk_market_value: 700 }
  assert.equal(overviewTotal([live, zero], 'equity'), 100)
  assert.equal(overviewTotal([old], 'equity'), null)
  assert.equal(overviewTotal([live, old], 'equity'), null)
  assert.equal(overviewTotal([live, usableOnly], 'equity'), null)
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
