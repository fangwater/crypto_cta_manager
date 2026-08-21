import type { TargetPosition } from './lib/targetPositions'

export type { TargetPosition }

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

export interface DashboardAccount {
  source_id: string
  /** Manager display name. Uses alias when configured. */
  account: string
  venue: string
  enabled: boolean
  gateway_prefix: string | null
  configurable: boolean
  live_equity_usdt?: number | null
  live_equity_status?: 'ok' | 'stale' | string | null
}

export interface OrderParameters {
  single_order_usdt: number
  orders_per_batch: number
  max_batch: number
  maker_price_anchor: 'own_best' | 'opposite_best_plus_one_tick'
  tick_spacing: number
  batch_interval_ms: number
  maker_timeout_ms: number
  max_maker_requotes: number
  target_tolerance_usdt: number
}

export interface OrderStrategyView {
  source_id: string
  strategy_name: string
  order_parameters: OrderParameters
  updated_at_us: number | null
  target_count: number
  nonzero_target_count: number
}

export interface OrderStrategyList {
  source_id: string
  strategies: string[]
}

export interface PositionStrategy {
  strategy_name: string
  targets: Record<string, TargetPosition>
  updated_at_us: number
}

export interface BindingPublishResult {
  source_id: string
  binding_name: string
  shares: number
  published?: OrderStrategyView | null
  error?: string | null
}

export interface SavedPositionStrategy extends PositionStrategy {
  publishes?: BindingPublishResult[]
}

export interface CatalogOrderStrategy {
  strategy_name: string
  order_parameters: OrderParameters
  updated_at_us: number
}

export interface AccountBinding {
  source_id: string
  binding_name: string
  position_strategy_name: string
  order_strategy_name: string
  shares: number
  updated_at_us: number
}

export interface AccountStudio {
  source_id: string
  /** NAV estimated trading fee rate as a fraction (e.g. 0.0004 = 4 bps). */
  estimated_fee_rate: number
  bindings: AccountBinding[]
}

export interface SavedSymbolContractLeverage {
  source_id: string
  symbol: string
  contract_leverage: number
  exchange: string
  endpoint: string
  http_status: number
  recorded_contract_leverage?: number | null
}

export interface DashboardSnapshot {
  generated_at_us: number
  generation_duration_ms: number
  refresh_interval_secs: number
  accounts?: DashboardAccount[]
  report: NavReport
}

export interface NavTimelinePoint extends NavTotals {
  ts_us: number
}

export interface SymbolNavTimeline {
  symbol: string
  points: NavTimelinePoint[]
}

export interface StrategyNavTimeline {
  strategy: string
  symbol_count: number
  gross_position_value_quote: number
  net_position_value_quote: number
  summary: NavTotals
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
  available_strategies: string[]
  summary: NavTotals
  symbols: AggregateSymbolNavReport[]
  points: NavTimelinePoint[]
  symbol_points: SymbolNavTimeline[]
  strategy_points: StrategyNavTimeline[]
  sampled: boolean
}

export interface TimelineSnapshot {
  generated_at_us: number
  generation_duration_ms: number
  report: NavTimelineReport
}

export interface ExecutionCostTotals {
  intended_qty: number
  filled_qty: number
  arrival_notional_usdt: number
  twap_notional_usdt: number
  actual_notional_usdt: number
  twap_cost_before_fee_usdt: number
  actual_cost_before_fee_usdt: number
}

export interface SymbolExecutionCost {
  symbol: string
  template_qty: number
  published_qty: number
  snapshot_qty: number
  intended_qty: number
  filled_qty: number
  fill_count: number
  minute_bar_count: number
  missing_minute_bar_count: number
  arrival_mid: number | null
  twap_mid: number | null
  actual_vwap: number | null
  arrival_notional_usdt: number | null
  twap_notional_usdt: number | null
  actual_notional_usdt: number | null
  twap_cost_before_fee_usdt: number | null
  actual_cost_before_fee_usdt: number | null
}

export interface AccountExecutionCost {
  source_id: string
  binding_name: string
  shares: number
  snapshot_ts_ms: number | null
  position_ready: boolean | null
  totals: ExecutionCostTotals
  symbols: SymbolExecutionCost[]
}

export interface PositionUpdateExecutionCost {
  received_at_us: number
  seq: number
  schema_version: number
  strategy_name: string
  window_start_us: number
  window_end_us: number
  skipped_legacy: boolean
  totals: ExecutionCostTotals
  accounts: AccountExecutionCost[]
}

export interface ExecutionCostReport {
  generated_at_us: number
  window_secs: number
  twap_secs: number
  price_basis: string
  fee_basis: string
  start_received_at_us: number
  end_received_at_us: number | null
  source_ids: string[]
  strategy_name: string | null
  update_count: number
  skipped_legacy_update_count: number
  totals: ExecutionCostTotals
  updates: PositionUpdateExecutionCost[]
}

export interface ExecutionCostSnapshot {
  generated_at_us: number
  generation_duration_ms: number
  report: ExecutionCostReport
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
export type TimelineChartMode = 'portfolio' | 'symbols' | 'strategies'
export type NavSeriesKey =
  | 'nav_change_before_fee_quote'
  | 'nav_change_after_fee_quote'
  | 'realized_pnl_before_fee_quote'
  | 'floating_pnl_quote'
  | 'estimated_trading_fee_quote'
export type SymbolRow = AggregateSymbolNavReport & {
  venues?: VenueNavReport[]
}
