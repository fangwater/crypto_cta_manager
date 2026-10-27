export interface OverviewRow {
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
  margin_percent: number | null
  date_ms: number | null
  queried_at_ms: number
  leverage_alert: boolean | null
  timeout_alert: boolean | null
  history_start_ms: number | null
  history_rebuilt_at_ms?: number | null
  account_kind?: 'cta' | 'fr'
  detail_url?: string | null
  collection_status?: 'live' | 'historical' | 'stale' | 'loading' | 'unavailable' | null
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
export function overviewTotal(rows: OverviewRow[], key: 'equity' | 'today_profit') {
  if (rows.some(row => row[key] == null || !Number.isFinite(row[key]) || row.account_kind === 'fr' && !['live', 'historical'].includes(row.collection_status ?? ''))) return null
  return rows.reduce((sum, row) => sum + row[key]!, 0)
}

export function overviewAlertCount(rows: OverviewRow[], key: 'leverage_alert' | 'timeout_alert') {
  if (rows.some(row => row[key] == null)) return null
  return rows.filter(row => row[key]).length
}
