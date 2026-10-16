import assert from 'node:assert/strict'
import test from 'node:test'
import { executionSymbolPlaceholder, isUnifiedVenue, symbolMatchesExecMarket } from '../src/format.ts'

test('native UTA account controls retain the USDT perpetual market', () => {
  for (const venue of ['bybit-futures', 'bitget-futures']) {
    assert.equal(isUnifiedVenue(venue), true)
    assert.equal(symbolMatchesExecMarket(venue, 'BTCUSDT'), true)
    for (const symbol of ['BTC', 'BTCUSD', 'BTCUSDC', 'BTCPERP', 'BTCUSDT_261225']) {
      assert.equal(symbolMatchesExecMarket(venue, symbol), false)
    }
    assert.equal(executionSymbolPlaceholder(venue), 'BTCUSDT / ETHUSDT')
  }
})
