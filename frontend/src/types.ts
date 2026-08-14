export interface NavTotals {
  fill_count: number
  volume_quote: number
  realized_pnl_before_fee_quote: number
  estimated_trading_fee_quote: number
  realized_pnl_after_fee_quote: number
  floating_pnl_quote: number
  nav_change_before_fee_quote: number
  nav_change_after_fee_quote: number
}

export interface VenueNavReport extends NavTotals {
  venue_code: number
  venue: string
  mark_price: number
  mark_price_source: 'latest_fill' | 'initial_snapshot' | 'override'
  initial_quantity: number
  initial_reference_price: number | null
  initial_reference_price_source: 'configured' | 'first_fill' | null
  long_quantity: number
  short_quantity: number
  net_quantity: number
  long_position_value_quote: number
  short_position_value_quote: number
  net_position_value_quote: number
  first_fill_ts_us: number | null
  last_fill_ts_us: number | null
}

export interface SymbolNavReport extends NavTotals {
  symbol: string
  venue_count: number
  initial_net_quantity: number
  long_quantity: number
  short_quantity: number
  net_quantity: number
  long_position_value_quote: number
  short_position_value_quote: number
  net_position_value_quote: number
  venues: VenueNavReport[]
}

export interface AggregateSymbolNavReport extends NavTotals {
  symbol: string
  source_count: number
  venue_count: number
  initial_net_quantity: number
  long_quantity: number
  short_quantity: number
  net_quantity: number
  long_position_value_quote: number
  short_position_value_quote: number
  net_position_value_quote: number
}

export interface SourceNavReport extends NavTotals {
  source_id: string
  account: string
  configured_venue: string
  estimated_fee_rate: number
  initial_position_snapshot_ts_us: number | null
  initial_position_count: number
  order_event_count: number
  ignored_at_or_before_snapshot_event_count: number
  ignored_non_fill_event_count: number
  first_fill_ts_us: number | null
  last_fill_ts_us: number | null
  symbols: SymbolNavReport[]
}

export interface NavReport {
  valuation: string
  source_count: number
  aggregate: NavTotals & { symbols: AggregateSymbolNavReport[] }
  sources: SourceNavReport[]
}

export interface DashboardSnapshot {
  generated_at_us: number
  generation_duration_ms: number
  refresh_interval_secs: number
  report: NavReport
}

export interface NavTimelinePoint extends NavTotals {
  ts_us: number
}

export interface SymbolNavTimeline {
  symbol: string
  points: NavTimelinePoint[]
}

export interface NavTimelineReport {
  valuation: string
  earliest_start_ts_us: number
  start_ts_us: number
  end_ts_us: number
  selected_source_ids: string[]
  available_symbols: string[]
  selected_symbols: string[]
  summary: NavTotals
  symbols: AggregateSymbolNavReport[]
  points: NavTimelinePoint[]
  symbol_points: SymbolNavTimeline[]
  sampled: boolean
}

export interface TimelineSnapshot {
  generated_at_us: number
  generation_duration_ms: number
  report: NavTimelineReport
}

export interface HealthResponse {
  status: 'ok' | 'degraded'
  source_count: number
  generated_at_us: number
  last_attempt_at_us: number
  refresh_interval_secs: number
  last_refresh_error: string | null
}

export type FeeMode = 'after' | 'before'
export type ChartMode = 'nav' | 'exposure'
export type TimelineChartMode = 'portfolio' | 'symbols'
export type NavSeriesKey =
  | 'nav_change_before_fee_quote'
  | 'nav_change_after_fee_quote'
  | 'realized_pnl_before_fee_quote'
  | 'floating_pnl_quote'
  | 'estimated_trading_fee_quote'
export type SymbolRow = AggregateSymbolNavReport & {
  venues?: VenueNavReport[]
}
