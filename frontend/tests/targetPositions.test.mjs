import assert from 'node:assert/strict'
import test from 'node:test'
import { normalizeTargetMap, targetQty } from '../src/lib/targetPositions.ts'

test('catalog clear stays explicit when reading and resaving targets', () => {
  const targets = normalizeTargetMap({BTCUSDT: {qty: 'clear', signal: -1}, ETHUSDT: {qty: 0}, SOLUSDT: 2})
  assert.equal(targets.BTCUSDT.qty, 'clear')
  assert.equal(targetQty(targets.BTCUSDT), 0)
  assert.equal(targetQty(targets.SOLUSDT), 2)
  const saved = JSON.parse(JSON.stringify(targets))
  assert.equal(saved.BTCUSDT.qty, 'clear')
  assert.equal(saved.BTCUSDT.signal, -1)
  assert.equal(saved.ETHUSDT.qty, 0)
  assert.deepEqual(normalizeTargetMap({BTCUSDT: {qty: 'close'}}), {})
})
