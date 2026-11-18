import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import { ApiError, parseApiError } from '../src/lib/apiError.ts'
import { fetchTimeline } from '../src/lib/timelineRequest.ts'

test('cold NAV history is pending only for an explicit loading 503', () => {
  const pending = parseApiError(503, { error: '净值历史正在加载，请稍后重试', loading: true })
  assert.ok(pending instanceof ApiError)
  assert.equal(pending.historyLoading, true)
  assert.equal(pending.navPricing, undefined)
  for (const [status, payload] of [
    [503, { loading: false }], [503, null], [500, { loading: true }],
    [401, { loading: true }], [403, { loading: true }],
  ]) {
    assert.equal(parseApiError(status, payload).historyLoading, false)
  }
})

test('minute coverage retains its own range and loading state', () => {
  for (const loading of [true, false]) {
    const error = parseApiError(loading ? 503 : 422, {
      code: 'nav_pricing_unavailable', loading, available_from_us: 123456,
    })
    assert.deepEqual(error.navPricing, { loading, availableFromUs: 123456 })
    assert.equal(error.historyLoading, false)
  }
})

test('timeline requests expose pending history and a later retry returns factual NAV', async () => {
  const savedFetch = globalThis.fetch
  const fixture = readFileSync(new URL('./fixtures/nav-proto/timeline.pb', import.meta.url))
  let calls = 0
  globalThis.fetch = async () => ++calls === 1
    ? Response.json({ error: '净值历史正在加载，请稍后重试', loading: true }, { status: 503 })
    : new Response(fixture, { headers: { 'Content-Type': 'application/x-protobuf' } })
  const createError = (...args) => new ApiError(...args)
  try {
    await assert.rejects(fetchTimeline('/timeline', undefined, createError), error => {
      assert.ok(error instanceof ApiError)
      assert.equal(error.historyLoading, true)
      assert.equal(error.navPricing, undefined)
      return true
    })
    const ready = await fetchTimeline('/timeline', undefined, createError)
    assert.ok(ready.report.points.length > 0)
    assert.equal(calls, 2)
  } finally { globalThis.fetch = savedFetch }
})
