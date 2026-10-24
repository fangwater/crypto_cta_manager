import assert from 'node:assert/strict'
import test from 'node:test'
import { displayedFunding, usesActualFunding, strategyFundingPoints } from '../src/lib/fundingPresentation.ts'
import { fundingMoney } from '../src/format.ts'

const actual = {
  summary: { enabled: true, start_ts_us: 1000, record_count: 2, income_quote: -0.023430743798 },
  total_nav_after_fee_quote: 9.976569256202,
  points: [],
}
const estimate = { income_quote: -0.01564985954644, total_nav_after_fee_quote: 9.984350140454, points: [] }

test('account totals select actual or estimated funding by the collection switch, including RapidX', () => {
  const shown = displayedFunding('account', actual, estimate)
  assert.equal(shown.kind, 'actual')
  assert.equal(shown.income, actual.summary.income_quote)
  assert.equal(shown.value.total_nav_after_fee_quote, actual.total_nav_after_fee_quote)
  const paused = { ...actual, summary: { ...actual.summary, enabled: false } }
  assert.equal(usesActualFunding(actual.summary), true)
  assert.equal(usesActualFunding(paused.summary), false)
  const pausedShown = displayedFunding('account', paused, estimate)
  assert.equal(pausedShown.kind, 'estimated')
  assert.equal(pausedShown.income, estimate.income_quote)
  assert.equal(pausedShown.value.total_nav_after_fee_quote, estimate.total_nav_after_fee_quote)
  assert.equal(displayedFunding('account', paused, undefined).kind, 'actual')
  assert.equal(displayedFunding('account', paused, { income_quote: null }).kind, 'actual')
  const pending = { ...actual, summary: { enabled: true, start_ts_us: null, income_quote: 0 } }
  assert.equal(displayedFunding('account', pending, estimate).income, estimate.income_quote)
  assert.equal(displayedFunding('account', pending, undefined).kind, 'unavailable')
})

for (const days of [60, 90]) {
  test(`${days}D prefers saved overlapping income over partial estimates, without summing both`, () => {
    const end = Date.UTC(2026, 9, 24) * 1_000
    const points = [{ ts_us: end - days * 86_400_000_000 }, { ts_us: end }]
    const paused = { ...actual, summary: { ...actual.summary, enabled: false } }
    paused.points = points
    const partial = { ...estimate, points, unavailable_reason: '仅最近 30 天有公开费率' }
    assert.equal(displayedFunding('account', paused, partial).income, actual.summary.income_quote)
    assert.equal(displayedFunding('strategy', paused, partial).income, partial.income_quote)
    assert.equal(displayedFunding('account', undefined, partial).income, partial.income_quote)
    const noOverlap = { ...paused, summary: { ...paused.summary, record_count: 0, income_quote: 0 } }
    assert.equal(displayedFunding('account', noOverlap, partial).kind, 'estimated')
    assert.equal(displayedFunding('account', noOverlap, undefined).kind, 'unavailable')
  })
}

test('a confirmed overlapping observation may report zero, but an unknown window may not', () => {
  const observed = { ...actual,
    summary: { ...actual.summary, record_count: 0, income_quote: 0,
      symbols: [{ start_ts_us: 2000, synced_through_ts_us: 3000 }] },
    points: [{ ts_us: 1000 }, { ts_us: 4000 }],
  }
  assert.equal(displayedFunding('account', observed, undefined).income, 0)
  assert.equal(displayedFunding('account', { ...observed, points: [{ ts_us: 4000 }] }, undefined).kind, 'unavailable')
})

test('strategy funding remains estimated even when a factual statement differs', () => {
  const shown = displayedFunding('strategy', actual, estimate)
  assert.equal(shown.kind, 'estimated')
  assert.equal(shown.income, estimate.income_quote)
  assert.equal(shown.value, estimate)
  assert.equal(displayedFunding('account', undefined, estimate).kind, 'estimated')
  assert.equal(displayedFunding('account', undefined, { income_quote: null }).kind, 'unavailable')
  assert.equal(displayedFunding('account', undefined, { income_quote: 0 }).kind, 'estimated')
})

test('opposite strategy funding stays visible independently of aggregate NAV and fees', () => {
  const strategy = { points: [{ ts_us: 1000, nav_change_before_fee_quote: 100, nav_change_after_fee_quote: 99 }] }
  const long = { points: [{ ts_us: 1000, funding_income_quote: -0.000008, total_nav_before_fee_quote: 99.999992, total_nav_after_fee_quote: 98.999992 }] }
  const short = { points: [{ ts_us: 1000, funding_income_quote: 0.000008, total_nav_before_fee_quote: 100.000008, total_nav_after_fee_quote: 99.000008 }] }
  assert.equal(long.points[0].funding_income_quote + short.points[0].funding_income_quote, 0)
  for (const fee of ['before', 'after']) {
    assert.deepEqual(strategyFundingPoints(strategy, long, fee, true), [[1, -0.000008]])
    assert.deepEqual(strategyFundingPoints(strategy, short, fee, true), [[1, 0.000008]])
  }
  assert.deepEqual(strategyFundingPoints(strategy, long, 'after', false), [[1, 98.999992]])
  assert.deepEqual(strategyFundingPoints(strategy, undefined, 'after', true), [])
  assert.deepEqual(strategyFundingPoints(strategy, undefined, 'after', false), [[1, 99]])
  assert.equal(fundingMoney(-0.000008), '-0.000008')
  assert.equal(fundingMoney(0.00000001), '0.00000001')
})
