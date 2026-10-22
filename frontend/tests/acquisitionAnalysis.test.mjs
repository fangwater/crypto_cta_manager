import assert from 'node:assert/strict'
import test from 'node:test'
import { analyzePricePath, deltaKey, priceFormatter } from '../src/lib/acquisitionAnalysis.ts'

function row(prices, overrides = {}) {
  return {
    sample_prices: prices,
    zero_volume_fallback_sample_count: 0,
    delta_qty: 1,
    virtual_vwap: 100,
    actual_vwap: 101,
    ...overrides,
  }
}

test('small coin prices and sub-cent differences remain distinguishable', () => {
  for (const [first, second] of [[0.000012345, 0.000012346], [0.12456, 0.12457], [123456.1234, 123456.1235]]) {
    const format = priceFormatter([first, second])
    assert.notEqual(format(first), format(second))
    assert.ok(Math.abs(Number(format(first).replaceAll(',', '')) - first) < Math.abs(second - first) / 10)
  }
  const format = priceFormatter([1e-15, 1.001e-15])
  assert.notEqual(format(1e-15), format(1.001e-15))
  assert.equal(format(null), '--')
  assert.equal(format(NaN), '--')
})

test('detects rise/reversal and fall/recovery independently of coin price scale', () => {
  for (const scale of [1, 1e-7, 1000]) {
    for (const values of [[100, 101, 102, 101, 100], [100, 99, 98, 99, 100]]) {
      const result = analyzePricePath(row(values.map((v) => v * scale)))
      assert.equal(result.complete, true)
      assert.equal(result.reversal.minute, 3)
      assert.ok(result.reversal.outwardBps * result.reversal.returnBps < 0)
      assert.ok(Math.min(Math.abs(result.reversal.outwardBps), Math.abs(result.reversal.returnBps)) > 190)
    }
  }
})

test('monotonic trends and flat windows are not reversals', () => {
  assert.equal(analyzePricePath(row([100, 101, 102, 103, 104])).reversal, null)
  const flat = analyzePricePath(row([100, 100, 100, 100, 100]))
  assert.equal(flat.reversal, null)
  assert.equal(flat.jumpBps, 0)
})

test('skipped minutes are never bridged to infer a one-minute jump or reversal', () => {
  const result = analyzePricePath(row([100, null, 120, 120, 120]))
  assert.equal(result.complete, false)
  assert.equal(result.reversal, null)
  assert.equal(result.jumpBps, 0)
  assert.equal(analyzePricePath(row([100, null, 120, null, 100])).jumpBps, null)
})

test('all-empty close fallback is excluded from traded-price pattern analysis', () => {
  const result = analyzePricePath(row([100, 110, 120, 110, 100], { zero_volume_fallback_sample_count: 5 }))
  assert.equal(result.complete, false)
  assert.equal(result.reversal, null)
  assert.equal(result.jumpBps, null)
})

test('cost decomposition is additive and flips signs for sells', () => {
  for (const delta_qty of [2, -2]) {
    const result = analyzePricePath(row([99, 100, 101, 100, 100], { delta_qty }))
    assert.ok(Math.abs(result.versusFirstBps + result.pathBps - Math.sign(delta_qty) * 100) < 1e-8)
  }
  assert.equal(analyzePricePath(row([100, 100, 100, 100, 100], { actual_vwap: null })).versusFirstBps, null)
})

test('delta selection identity stays isolated by source, venue, strategy and binding', () => {
  const base = { source_id: 'a', binding_name: 'b', strategy_name: 'c', symbol: 'BTCUSDT', venue: 'binance-futures', received_at_us: 123 }
  for (const field of Object.keys(base)) assert.notEqual(deltaKey(base), deltaKey({ ...base, [field]: 'different' }))
})


test('three-minute RapidX paths use the third minute as the endpoint', () => {
  const reverse = analyzePricePath(row([100, 102, 101]))
  assert.equal(reverse.complete, true)
  assert.equal(reverse.reversal.minute, 2)
  assert.ok(reverse.reversal.returnBps < 0)
  assert.equal(analyzePricePath(row([100, 101, 102])).reversal, null)
  assert.equal(analyzePricePath(row([100, null, 102])).complete, false)
  assert.equal(analyzePricePath(row([])).complete, false)
})

test('instant taker uses one open price without inventing a market path', () => {
  for (const delta_qty of [2, -2]) {
    const result = analyzePricePath(row([100], { execution_model: 'instant_taker', delta_qty }))
    assert.equal(result.complete, false)
    assert.equal(result.jumpBps, null)
    assert.equal(result.reversal, null)
    assert.ok(result.pathBps === 0)
    assert.equal(result.versusFirstBps, Math.sign(delta_qty) * 100)
  }
})
