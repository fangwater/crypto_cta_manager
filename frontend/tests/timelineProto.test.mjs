import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { create, fromBinary, toBinary } from '@bufbuild/protobuf'
import { CurveKind, Metric, TimelineSchema } from '../src/generated/timeline_pb.js'
import { decodeTimeline } from '../src/lib/timelineProto.ts'

const fixtureDir = new URL('./fixtures/nav-proto/', import.meta.url)
const original = JSON.parse(readFileSync(new URL('timeline.json', fixtureDir), 'utf8'))
const bytes = readFileSync(new URL('timeline.pb', fixtureDir))
const actual = decodeTimeline(bytes)
const fields = ['ts_us', 'gross_position_value_quote', 'net_position_value_quote',
  'realized_pnl_before_fee_quote', 'floating_pnl_quote', 'estimated_trading_fee_quote',
  'nav_change_before_fee_quote', 'nav_change_after_fee_quote']

test('Rust encoder and browser decoder retain exact plotted doubles, timestamps and interval totals', () => {
  assert.deepEqual(actual.report.summary, original.report.summary)
  assert.equal(actual.generated_at_us, original.generated_at_us)
  assert.deepEqual(actual.report.points, original.report.points.map(point => Object.fromEntries(fields.map(key => [key, point[key]]))))
  assert.equal(actual.report.points[1].estimated_trading_fee_quote, -1e-12)
  assert.equal(actual.estimated_funding.income_quote, null)
  assert.equal(actual.estimated_funding.unavailable_reason, null)
  assert.deepEqual(actual.report.selected_source_ids, ['account'])
})

test('one shared time axis, empty symbol omitted, tiny carried position retained without fills', () => {
  const wire = fromBinary(TimelineSchema, bytes)
  assert.equal(wire.axes.length, 1)
  assert.deepEqual(actual.report.symbol_points.map(row => row.symbol), ['BTCUSDT', 'CARRYUSDT'])
  assert.equal(actual.report.symbol_points[1].points[0].net_position_value_quote, -1e-15)
  assert.equal(actual.report.strategy_points[0].has_activity, true)
  assert.deepEqual(actual.report.strategy_points[0].points, [])
  assert.equal(Object.hasOwn(actual.report.points[0], 'fill_count'), false)
})

test('reject truncated protobuf, invalid columns and duplicated curves', () => {
  assert.throws(() => decodeTimeline(Uint8Array.of(255)))
  const invalid = fromBinary(TimelineSchema, bytes)
  invalid.curves[0].columns[0].values = [1, 2, 3]
  assert.throws(() => decodeTimeline(toBinary(TimelineSchema, invalid)), /数值列格式错误/)
  invalid.curves[0].columns[0].values = [1]
  invalid.curves.push(invalid.curves[0])
  assert.throws(() => decodeTimeline(toBinary(TimelineSchema, invalid)), /重复曲线/)
})

test('reject missing axes and unsafe timestamps instead of rounding financial sample times', () => {
  const invalid = fromBinary(TimelineSchema, bytes)
  invalid.curves[0].axis = 999
  assert.throws(() => decodeTimeline(toBinary(TimelineSchema, invalid)), /缺少时间轴/)
  invalid.curves[0].axis = 0
  invalid.axes[0].startUs = 9007199254740992n
  assert.throws(() => decodeTimeline(toBinary(TimelineSchema, invalid)), /精确数值范围/)
})

test('a theoretical-only response stays independent of factual reports', () => {
  const wire = create(TimelineSchema, {
    metadata: { generated_at_us: 1000, generation_duration_ms: 1, theoretical: [{
      points: [], loading: false, unavailable_reason: null,
    }], execution_slippage: [] },
    axes: [{ startUs: 1000n, deltasUs: [1n] }],
    curves: [{ kind: CurveKind.THEORETICAL, key: '0', axis: 0, columns: [
      { metric: Metric.NAV_BEFORE_FEE, values: [0, 3.125] },
      { metric: Metric.NAV_AFTER_FEE, values: [0, 3.126] },
      { metric: Metric.TRADING_FEE, values: [-0.001] },
    ] }],
  })
  const decoded = decodeTimeline(toBinary(TimelineSchema, wire))
  assert.equal(decoded.report, undefined)
  assert.equal(decoded.theoretical[0].points[1].nav_change_after_fee_quote, 3.126)
  assert.equal(decoded.theoretical[0].points[1].estimated_trading_fee_quote, -0.001)
})

test('production request adapter fetches theory alone and retains factual NAV and null funding', async () => {
  const { requestNavTimeline } = await import('../src/lib/timelineBundleClient.ts')
  class ApiError extends Error {
    constructor(message, status, navPricing) { super(message); this.status = status; this.navPricing = navPricing }
  }
  const calls = []
  const savedFetch = globalThis.fetch
  const baseWire = fromBinary(TimelineSchema, bytes)
  baseWire.metadata.theoretical[0].loading = true
  const overlay = create(TimelineSchema, {
    metadata: { generated_at_us: 1000, generation_duration_ms: 3,
      theoretical: [{ points: [], loading: false, unavailable_reason: null }], execution_slippage: [] },
  })
  globalThis.fetch = async (url, options) => {
    calls.push({ url, accept: options.headers.Accept })
    return new Response(toBinary(TimelineSchema, calls.length === 1 ? baseWire : overlay),
      { headers: { 'Content-Type': 'application/x-protobuf' } })
  }
  try {
    const query = { startMs: 1, endMs: 2, sourceIds: ['adapter-account'], view: 'symbols' }
    const factual = await requestNavTimeline('/timeline', { ...query, includeTheoretical: false }, ApiError)
    const combined = await requestNavTimeline('/timeline', { ...query, includeTheoretical: true }, ApiError)
    assert.equal(calls.length, 2)
    assert.match(calls[0].url, /view=symbols/)
    assert.match(calls[1].url, /view=theoretical/)
    assert.equal(calls[0].accept, 'application/x-protobuf')
    assert.equal(combined.report, factual.report)
    assert.equal(combined.estimated_funding.income_quote, null)
    assert.equal(combined.theoretical[0].loading, false)
  } finally { globalThis.fetch = savedFetch }
})
