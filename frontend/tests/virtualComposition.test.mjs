import assert from 'node:assert/strict'
import test from 'node:test'
import { canAddVirtualChild, composeVirtual } from '../src/lib/virtualComposition.ts'

const leaf = (shares, overrides = {}) => ({ binding_name: 'signal', position_strategy_name: 'position', order_strategy_name: 'maker', shares, ...overrides })
const child = (virtual_id, multiplier) => ({ virtual_id, multiplier })
const account = (virtual_id, expanded_bindings, children = []) => ({ virtual_id, expanded_bindings, children })

test('nested and shared contributions multiply and add without mutating server templates', () => {
  const a = account('a', [leaf(2)])
  const b = account('b', composeVirtual([leaf(1)], [child('a', 3)], [a]), [child('a', 3)])
  const accounts = [a, b]
  const before = structuredClone(accounts)
  assert.deepEqual(composeVirtual([], [child('b', 4), child('a', 0.5)], accounts), [leaf(29)])
  assert.deepEqual(composeVirtual([], [child('a', 0.5), child('b', 4)], accounts), [leaf(29)])
  assert.deepEqual(composeVirtual([], [child('b', 0)], accounts), [leaf(0)])
  assert.deepEqual(accounts, before)
})

test('selector excludes self and transitive ancestors including zero-weight edges', () => {
  const accounts = [account('a', [], [child('b', 0)]), account('b', [], [child('c', 1)]), account('c', [])]
  assert.equal(canAddVirtualChild(accounts, 'c', 'c'), false)
  assert.equal(canAddVirtualChild(accounts, 'c', 'a'), false)
  assert.equal(canAddVirtualChild(accounts, 'a', 'c'), true)
  assert.equal(canAddVirtualChild(accounts, '', 'a'), true)
  assert.equal(canAddVirtualChild(accounts, '', 'hidden'), false)
})

test('unavailable children, duplicate references, conflicting identities and invalid ratios block preview', () => {
  const accounts = [account('a', [leaf(2)])]
  assert.throws(() => composeVirtual([], [child('hidden', 1)], accounts), /不可见/)
  assert.throws(() => composeVirtual([], [child('a', 1), child('a', 0)], accounts), /重复/)
  assert.throws(() => composeVirtual([leaf(1), leaf(1)], [], accounts), /重复/)
  for (const overrides of [{ order_strategy_name: 'taker' }, { position_strategy_name: 'other' }]) {
    assert.throws(() => composeVirtual([leaf(1, overrides)], [child('a', 0)], accounts), /冲突/)
  }
  for (const ratio of [-1, NaN, Infinity, Number.MAX_VALUE]) {
    assert.throws(() => composeVirtual([], [child('a', ratio)], accounts))
  }
  assert.throws(() => composeVirtual([leaf(Number.MAX_VALUE)], [child('a', Number.MAX_VALUE / 2)], accounts), /超出/)
})
