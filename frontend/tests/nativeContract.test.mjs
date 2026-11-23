import assert from 'node:assert/strict'
import test from 'node:test'
import { marketLabel, nativeContract } from '../src/lib/nativeContract.ts'

const now = Date.UTC(2026, 10, 23)

test('domestic contracts drop the venue and use the Viz Chinese names', () => {
  assert.deepEqual(nativeContract('SHFE:rb2701', now), { name: '螺纹钢 · 2027年01月', code: 'rb2701' })
  assert.deepEqual(nativeContract('GFEX:lc2701', now), { name: '碳酸锂 · 2027年01月', code: 'lc2701' })
  assert.deepEqual(nativeContract('DCE:pg2701', now), { name: '液化石油气 · 2027年01月', code: 'pg2701' })
  // CZCE YMM codes take the decade nearest to the reference year.
  assert.deepEqual(nativeContract('CZCE:AP701', now), { name: '苹果 · 2027年01月', code: 'AP701' })
  assert.equal(nativeContract('CZCE:SR509', Date.UTC(2029, 11, 1))?.name, '白糖 · 2025年09月')
  assert.equal(nativeContract('INE:sc2612C780', now)?.name, '原油 · 2026年12月 · 看涨 · 行权价 780')
  assert.equal(marketLabel('SHFE:ss2701', now), '不锈钢 · 2027年01月（ss2701）')
})

test('Crypto symbols and unknown contracts stay unchanged', () => {
  for (const symbol of ['BTCUSDT', 'ETHUSDC', 'SHFE:xx2701', 'NYMEX:cl2701', 'SHFE:rb2713', 'rb2701']) {
    assert.equal(nativeContract(symbol, now), null, symbol)
    assert.equal(marketLabel(symbol, now), symbol)
  }
})
