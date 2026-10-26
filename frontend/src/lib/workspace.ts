export function accountVenueLabel(venue: string) {
  const exchange = venue.includes('binance') ? '币安' : venue.includes('okex') || venue.includes('okx') ? '欧易' : venue.includes('bybit') ? 'Bybit' : venue.includes('bitget') ? 'Bitget' : venue.includes('gate') ? 'Gate' : venue
  return `${exchange} · ${['binance', 'bybit', 'gate', 'bitget'].includes(venue) ? '现货 / 合约' : venue === 'binance-coin-futures' ? '币本位' : 'U 本位'}`
}
