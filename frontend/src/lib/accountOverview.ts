import type { SourceSyncState } from '../types'

export interface OverviewRow extends SourceSyncState {
  source_id: string
  account: string
  venue: string
  yesterday_equity: number | null
  equity: number | null
  today_profit: number | null
  profit_percent: number | null
  long_position: number | null
  short_position: number | null
  long_leverage: number | null
  short_leverage: number | null
  net_position: number | null
  position_unpriced_assets?: string[]
  max_drawdown_15d_percent: number | null
  margin: number | null
  margin_kind?: 'initial' | 'ltp_frozen' | null
  margin_percent: number | null
  date_ms: number | null
  queried_at_ms: number
  leverage_alert: boolean | null
  timeout_alert: boolean | null
  history_start_ms: number | null
  history_rebuilt_at_ms?: number | null
  account_kind?: 'cta' | 'fr' | 'mm'
  detail_url?: string | null
  position_status?: 'live' | 'historical' | null
  position_as_of_ms?: number | null
  position_historical_reason?: string | null
  quote_currency?: 'CNY' | null
  funds_as_of_ms?: number | null
  funds_status?: 'live' | 'historical' | null
  funds_historical_reason?: string | null
  funds_trading_day?: string | null
  sdk_market_value?: number | null
}

export interface AccountOverview {
  accounts: OverviewRow[]
  dates: string[]
  generated_at_ms: number
  loading: boolean
  alerts: { long_leverage_limit: number | null; short_leverage_limit: number | null; timeout_secs: number | null }
  external_status?: Array<{ provider_id: string; status: string; last_success_ms: number | null }>
}

// A partial sum would look like a factual total. Keep an incomplete total unknown.
export function isExternalOverview(row: OverviewRow) {
  return row.source_id?.startsWith('external:') || row.account_kind === 'fr' || row.account_kind === 'mm'
}

const syncBadges = {
  live: { label: '已同步', tone: 'success' },
  loading: { label: '同步中', tone: 'neutral' },
  stale: { label: '同步延迟', tone: 'warning' },
  unavailable: { label: '同步不可用', tone: 'warning' },
  historical: { label: '历史数据', tone: 'neutral' },
} as const

export function nativeSourceStatus(source: SourceSyncState) {
  const sync = (source.collection_status && syncBadges[source.collection_status])
    || { label: '等待同步', tone: 'neutral' } as const
  const title = source.collection_status_reason ?? sync.label
  if (source.execution_status === 'stopped') return { label: '已停止', tone: 'neutral', title } as const
  if (source.execution_status === 'disconnected') return { label: '已断线', tone: 'warning', title } as const
  if (source.execution_status === 'recovering' && source.collection_status === 'live') return { label: '对账中', tone: 'neutral', title } as const
  return { ...sync, title }
}

export function overviewTotal(rows: OverviewRow[], key: 'equity' | 'today_profit') {
  if (rows.some(row => row[key] == null || !Number.isFinite(row[key])
    || row.funds_status === 'historical'
    || isExternalOverview(row) && !['live', 'historical'].includes(row.collection_status ?? ''))) return null
  return rows.reduce((sum, row) => sum + row[key]!, 0)
}

export function overviewAlertCount(rows: OverviewRow[], key: 'leverage_alert' | 'timeout_alert') {
  if (rows.some(row => row[key] == null)) return null
  return rows.filter(row => row[key]).length
}
