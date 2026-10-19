import type { DashboardAccount, DashboardSnapshot, SourceNavReport } from '../types'

export function workspaceAccounts(dashboard: DashboardSnapshot | null): DashboardAccount[] {
  if (!dashboard) return []
  // An explicitly empty catalog is authoritative for this session.
  return dashboard.accounts ?? dashboard.report.sources.map(source => ({
    source_id: source.source_id, account: source.account, venue: source.configured_venue,
    enabled: true, gateway_prefix: null, configurable: false,
  }))
}

export function accountVenueLabel(venue: string) {
  const exchange = venue.includes('binance') ? '币安' : venue.includes('okex') || venue.includes('okx') ? '欧易' : venue.includes('bybit') ? 'Bybit' : venue.includes('bitget') ? 'Bitget' : venue.includes('gate') ? 'Gate' : venue
  return `${exchange} · ${venue === 'binance-coin-futures' ? '币本位' : 'U 本位'}`
}

export function accountMetrics(account: DashboardAccount, report?: SourceNavReport) {
  const long = report ? report.symbols.reduce((sum, symbol) => sum + symbol.long_position_value_quote, 0) : null
  const short = report ? report.symbols.reduce((sum, symbol) => sum + Math.abs(symbol.short_position_value_quote), 0) : null
  const equity = account.live_equity_usdt != null && Number.isFinite(account.live_equity_usdt) ? account.live_equity_usdt : null
  // USD coin-margined values must not be divided by USDT equity without FX.
  const comparable = account.venue !== 'binance-coin-futures' && equity !== null && equity > 0 && account.live_equity_status === 'ok'
  return {
    equity, long, short,
    net: long !== null && short !== null ? long - short : null,
    longLeverage: comparable && long !== null ? long / equity : null,
    shortLeverage: comparable && short !== null ? short / equity : null,
    pnl: report?.nav_change_after_fee_quote ?? null,
  }
}
