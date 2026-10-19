import assert from 'node:assert/strict'
import test from 'node:test'
import { createServerClock } from '../src/lib/serverClock.ts'

const serverTime = Date.UTC(2026, 9, 16, 5, 8, 37)
const dateHeader = new Date(serverTime).toUTCString()

test('1D defaults and refreshes stay at server time with fast or slow browser clocks', () => {
  for (const skew of [2300, 600_000, -600_000, 86_400_000]) {
    let monotonic = 500
    let local = serverTime + skew
    const clock = createServerClock(() => monotonic, () => local)
    clock.observe(dateHeader)
    assert.equal(clock.now(), serverTime)
    // The query continues advancing even if the OS wall clock jumps again.
    monotonic += 61_234.5
    local += 86_400_000
    assert.equal(clock.now(), serverTime + 61_234)
    clock.observe(new Date(serverTime + 61_000).toUTCString())
    assert.equal(clock.now(), serverTime + 61_000)
  }
})

test('response delay never moves an automatic query into the future', () => {
  let monotonic = 12_000
  const clock = createServerClock(() => monotonic, () => serverTime + 999_999)
  // Header generated before twelve seconds of transit/body setup delay.
  clock.observe(dateHeader)
  assert.equal(clock.now(), serverTime)
  monotonic += 250.9
  assert.equal(clock.now(), serverTime + 250)
  assert.ok(clock.now() <= serverTime + 12_250)
})

test('missing or invalid headers preserve the last usable server clock', () => {
  const clock = createServerClock(() => 100, () => 1234)
  clock.observe(null)
  clock.observe('invalid')
  assert.equal(clock.now(), 1234)
  clock.observe(dateHeader)
  for (const header of [null, '', 'invalid']) {
    clock.observe(header)
    assert.equal(clock.now(), serverTime)
  }
})
