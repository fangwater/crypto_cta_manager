import assert from 'node:assert/strict'
import test from 'node:test'
import { retainedQuickRangeStartMs } from '../src/lib/navRange.ts'

const minute = 60_000
const day = 24 * 60 * minute
const serverNow = Date.UTC(2026, 9, 15, 3, 30, 21)
const available = Date.UTC(2026, 8, 15, 3, 32)

test('30D uses the server retention boundary despite transport delay or a slow browser clock', () => {
  for (const clockDelay of [0, 1, 3 * minute, 20 * minute]) {
    const end = serverNow - clockDelay
    const start = end - 30 * day
    const next = retainedQuickRangeStartMs('30D', start, end, available * 1_000, 300)
    assert.equal(next, available + 5 * minute)
    // The complete pricing window of the first in-range slice remains retained.
    assert.ok(next - 5 * minute >= available)
    assert.ok(next < end)
  }
})

test('an already retained complete execution window and its exact boundary stay unchanged', () => {
  for (const start of [available + 5 * minute, available + 5 * minute + 1, serverNow - day]) {
    assert.equal(retainedQuickRangeStartMs('30D', start, serverNow, available * 1_000, 300), start)
  }
})

test('a long backfill realigns before its crossing execution window expires', () => {
  const start = retainedQuickRangeStartMs('30D', serverNow - 30 * day, serverNow, available * 1_000, 300)
  assert.equal(retainedQuickRangeStartMs('30D', start, serverNow, available * 1_000, 300), start)
  assert.equal(retainedQuickRangeStartMs('30D', start, serverNow, (available + 4 * minute) * 1_000, 300), available + 9 * minute)
  const advanced = available + 10 * minute
  assert.equal(retainedQuickRangeStartMs('30D', start, serverNow, advanced * 1_000, 300), advanced + 5 * minute)
})

test('custom, ALL, 1D and 7D historical ranges are never silently shortened', () => {
  const start = serverNow - 40 * day
  for (const range of [null, 'ALL', '1D', '7D']) {
    assert.equal(retainedQuickRangeStartMs(range, start, serverNow, available * 1_000, 300), start)
  }
})

test('an entirely expired range is not moved past its end or turned into a zero-duration window', () => {
  for (const end of [available - minute, available, available + 5 * minute]) {
    const start = end - 30 * day
    assert.equal(retainedQuickRangeStartMs('30D', start, end, available * 1_000, 300), start)
  }
})

test('disabled, incomplete or invalid availability metadata does not start a retry loop', () => {
  const start = serverNow - 30 * day
  for (const boundary of [null, NaN, Infinity, 0, -1]) {
    assert.equal(retainedQuickRangeStartMs('30D', start, serverNow, boundary, 300), start)
  }
  for (const window of [NaN, Infinity, 0, -1]) {
    assert.equal(retainedQuickRangeStartMs('30D', start, serverNow, available * 1_000, window), start)
  }
})
