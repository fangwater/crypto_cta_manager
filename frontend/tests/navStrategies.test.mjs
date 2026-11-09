import assert from 'node:assert/strict'
import test from 'node:test'
import { activeNavStrategies, hasNavStrategyActivity } from '../src/lib/navStrategies.ts'

const zeroTotals = {
  fill_count: 0, volume_quote: 0, maker_fill_count: 0, maker_volume_quote: 0,
  taker_fill_count: 0, taker_volume_quote: 0, unknown_liquidity_fill_count: 0,
  unknown_liquidity_volume_quote: 0, realized_pnl_before_fee_quote: 0,
  estimated_trading_fee_quote: 0, realized_pnl_after_fee_quote: 0,
  floating_pnl_quote: 0, nav_change_before_fee_quote: 0, nav_change_after_fee_quote: 0,
}
const point = (ts_us, values = {}) => ({
  ...zeroTotals, ts_us, gross_position_value_quote: 0, net_position_value_quote: 0, ...values,
})
const empty = (strategy) => ({
  strategy, symbol_count: 0, gross_position_value_quote: 0, net_position_value_quote: 0,
  summary: { ...zeroTotals }, points: [point(1000), point(2000)],
})
const funding = (strategy, income = 0) => ({
  strategy, income_quote: income, income_by_asset: { USDT: income },
  total_nav_before_fee_quote: income, total_nav_after_fee_quote: income,
  points: [{ ts_us: 1000, funding_income_quote: 0 }, { ts_us: 2000, funding_income_quote: income }],
})

test('empty legacy Prep strategies disappear from both selectors without modifying NAV data', () => {
  const old = ['1min', '5min', '15min'].map(period => empty(`cta-prep02-bn-${period}-pos`))
  const active = { ...empty('cta-top-5-prep02-bn-1min-pos'), summary: { ...zeroTotals, fill_count: 1 } }
  const timeline = {
    report: { available_strategies: [...old, active].map(row => row.strategy), strategy_points: [...old, active], summary: { fill_count: 1 } },
    estimated_funding: { strategies: old.map(row => funding(row.strategy)) },
  }
  const before = structuredClone(timeline)
  assert.deepEqual(activeNavStrategies(timeline), [active.strategy])
  assert.deepEqual(timeline, before)
  assert.deepEqual(activeNavStrategies(null), [])
})

test('an old strategy reappears in a historical window with fills even if its NAV and end position are zero', () => {
  const old = empty('cta-prep02-bn-5min-pos')
  old.summary.fill_count = 2
  assert.equal(hasNavStrategyActivity(old), true)
})

test('carried and offsetting strategy holdings remain visible without new fills or PnL', () => {
  for (const values of [
    { symbol_count: 1 }, { gross_position_value_quote: 100, net_position_value_quote: 0 },
    { net_position_value_quote: -100 }, { gross_position_value_quote: 1e-12 },
  ]) assert.equal(hasNavStrategyActivity({ ...empty('carried'), ...values }), true)
})

test('intrawindow PnL and exposure are retained when the endpoint returns to zero', () => {
  for (const values of [{ nav_change_after_fee_quote: 3 }, { gross_position_value_quote: 100 }]) {
    assert.equal(hasNavStrategyActivity({ ...empty('round-trip'), points: [point(1000), point(1500, values), point(2000)] }), true)
  }
})

test('funding-only, offsetting assets and small or subsequently reversed funding remain visible', () => {
  const row = empty('funding-only')
  const zero = funding(row.strategy)
  for (const estimate of [
    funding(row.strategy, -1e-12),
    { ...zero, income_by_asset: { USDT: 1, USDC: -1 } },
    { ...zero, points: [{ ts_us: 1000, funding_income_quote: 1 }, { ts_us: 2000, funding_income_quote: 0 }] },
  ]) assert.deepEqual(activeNavStrategies({ report: { available_strategies: [row.strategy], strategy_points: [row] }, estimated_funding: { strategies: [estimate] } }), [row.strategy])
})

test('unknown or incomplete values do not silently hide strategies', () => {
  assert.equal(hasNavStrategyActivity({ ...empty('unknown'), gross_position_value_quote: null }), true)
  assert.deepEqual(activeNavStrategies({ report: { available_strategies: ['pending'], strategy_points: [] } }), ['pending'])
})
