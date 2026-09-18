import type { TargetPosition } from './lib/targetPositions'

export type { TargetPosition }

export interface NavTotals {
  fill_count: number
  volume_quote: number
  maker_fill_count: number
  maker_volume_quote: number
  taker_fill_count: number
  taker_volume_quote: number
  unknown_liquidity_fill_count: number
  unknown_liquidity_volume_quote: number
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
  maker_fee_rate: number
  taker_fee_rate: number
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
  /** Current session's grant on this account. Undefined only in stale payloads. */
  access_level?: 'view' | 'configure' | null
  account_pnl_start_ts_us?: number | null
  strategy_pnl_start_ts_us?: number | null
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

export interface OrderParameterOverrides {
  single_order_usdt?: number
  orders_per_batch?: number
  max_batch?: number
  maker_price_anchor?: OrderParameters['maker_price_anchor']
  tick_spacing?: number
  batch_interval_ms?: number
  maker_timeout_ms?: number
  max_maker_requotes?: number
  target_tolerance_usdt?: number
}

export interface OrderStrategyView {
  source_id: string
  strategy_name: string
  order_parameters: OrderParameters
  symbol_overrides: Record<string, OrderParameterOverrides>
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
  symbol_order_strategy_overrides: Record<string, string>
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
  maker_fee_rate: number
  taker_fee_rate: number
  theoretical_twap_fee_rate: number
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
  gross_position_value_quote: number
  net_position_value_quote: number
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
  theoretical: TheoreticalNavTimeline
}

export interface TheoreticalNavPoint {
  ts_us: number
  nav_change_before_fee_quote: number
  nav_change_after_fee_quote: number
  estimated_trading_fee_quote: number
}

export interface TheoreticalNavTimeline {
  valuation: string
  execution_window_secs: number
  price_basis: string
  fee_basis: string
  available_from_us: number | null
  latest_point_ts_us: number | null
  points: TheoreticalNavPoint[]
  sampled: boolean
}

export interface ExecutionCostTotals {
  intended_qty: number
  filled_qty: number
  actual_fill_count: number
  arrival_notional_usdt: number
  twap_notional_usdt: number
  actual_notional_usdt: number
  twap_cost_before_fee_usdt: number
  actual_cost_before_fee_usdt: number
  estimated_trading_fee_usdt: number
  actual_cost_after_fee_usdt: number
  comparable_fill_count: number
  comparable_arrival_notional_usdt: number
  comparable_twap_notional_usdt: number
  actual_price_slippage_usdt: number
  twap_price_slippage_on_filled_usdt: number
  shortfall_vs_twap_usdt: number
  actual_slippage_bps: number
  twap_slippage_bps: number
  shortfall_vs_twap_bps: number
}

export interface ExecutionCostPoint {
  ts_us: number
  twap_cost_before_fee_usdt: number
  actual_cost_before_fee_usdt: number
  estimated_trading_fee_usdt: number
  actual_cost_after_fee_usdt: number
  actual_price_slippage_usdt: number
  twap_price_slippage_on_filled_usdt: number
  shortfall_vs_twap_usdt: number
  actual_slippage_bps: number | null
  twap_slippage_bps: number | null
  shortfall_vs_twap_bps: number | null
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
  estimated_trading_fee_usdt: number
  actual_cost_after_fee_usdt: number | null
  side: 'buy' | 'sell' | null
  actual_price_slippage_usdt: number | null
  twap_price_slippage_on_filled_usdt: number | null
  shortfall_vs_twap_usdt: number | null
  actual_slippage_bps: number | null
  twap_slippage_bps: number | null
  shortfall_vs_twap_bps: number | null
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
  actual_fee_basis: string
  start_received_at_us: number
  end_received_at_us: number | null
  source_ids: string[]
  strategy_name: string | null
  update_count: number
  execution_update_count: number
  page: number
  page_size: number
  page_count: number
  returned_update_count: number
  skipped_legacy_update_count: number
  totals: ExecutionCostTotals
  points: ExecutionCostPoint[]
  updates: PositionUpdateExecutionCost[]
}

export interface ExecutionCostSnapshot {
  generated_at_us: number
  generation_duration_ms: number
  report: ExecutionCostReport
}

export interface AcquisitionCostTotals {
  virtual_delta_count: number
  missing_virtual_delta_count: number
  comparable_delta_count: number
  virtual_turnover_usdt: number
  virtual_fee_usdt: number
  actual_fill_count: number
  actual_turnover_usdt: number
  actual_fee_usdt: number
  matched_virtual_turnover_usdt: number
  actual_matched_turnover_usdt: number
  actual_matched_fee_usdt: number
  matched_fill_count: number
  unmatched_fill_count: number
  unmatched_fill_notional_usdt: number
  opposite_fill_count: number
  opposite_fill_notional_usdt: number
  stale_reference_fill_count: number
  stale_reference_fill_notional_usdt: number
  price_shortfall_usdt: number
  first_mid_shortfall_usdt: number
  five_sample_drift_usdt: number
  fee_shortfall_usdt: number
  after_fee_shortfall_usdt: number
  price_shortfall_bps: number
  first_mid_shortfall_bps: number
  matched_turnover_coverage: number
  actual_fill_reference_coverage: number
}

export interface AcquisitionCostPoint {
  ts_us: number
  virtual_turnover_usdt: number
  actual_matched_turnover_usdt: number
  price_shortfall_usdt: number
  after_fee_shortfall_usdt: number
}

export interface AcquisitionCostBreakdown {
  bucket: string
  fill_count: number
  reference_turnover_usdt: number
  actual_turnover_usdt: number
  actual_fee_usdt: number
  virtual_fee_usdt: number
  price_shortfall_usdt: number
  first_mid_shortfall_usdt: number
  five_sample_drift_usdt: number
  price_shortfall_bps: number
  first_mid_shortfall_bps: number
  after_fee_shortfall_usdt: number
}

export interface AcquisitionFillDiagnostic {
  source_id: string
  strategy_name: string
  symbol: string
  target_received_at_us: number
  order_signal_ts_us: number
  fill_ts_us: number
  client_order_id: number
  side: string
  liquidity: string
  target_signal: number
  execution_mode: string
  actual_qty: number
  actual_price: number
  virtual_price: number
  target_delay_us: number
  order_delay_us: number
  reference_turnover_usdt: number
  price_shortfall_usdt: number
  first_mid_shortfall_usdt: number
  five_sample_drift_usdt: number
  price_shortfall_bps: number
}

export interface AcquisitionCostRow {
  source_id: string
  binding_name: string
  strategy_name: string
  symbol: string
  venue: string
  received_at_us: number
  virtual_execution_ts_us: number
  delta_qty: number
  sample_mids: [number, number, number, number, number]
  virtual_vwap: number
  virtual_turnover_usdt: number
  virtual_fee_usdt: number
  actual_matched_qty: number
  actual_vwap: number | null
  actual_matched_turnover_usdt: number
  actual_matched_fee_usdt: number
  matched_fill_count: number
  fill_ratio: number
  price_shortfall_usdt: number | null
  fee_shortfall_usdt: number | null
  after_fee_shortfall_usdt: number | null
  price_shortfall_bps: number | null
}

export interface AcquisitionCostReport {
  generated_at_us: number
  price_basis: string
  fee_basis: string
  start_received_at_us: number
  end_received_at_us: number
  source_ids: string[]
  strategy_name: string | null
  page: number
  page_size: number
  page_count: number
  returned_row_count: number
  totals: AcquisitionCostTotals
  points: AcquisitionCostPoint[]
  by_symbol: AcquisitionCostBreakdown[]
  by_side: AcquisitionCostBreakdown[]
  by_liquidity: AcquisitionCostBreakdown[]
  by_target_delay: AcquisitionCostBreakdown[]
  by_order_delay: AcquisitionCostBreakdown[]
  by_symbol_liquidity: AcquisitionCostBreakdown[]
  by_symbol_target_delay: AcquisitionCostBreakdown[]
  by_target_signal: AcquisitionCostBreakdown[]
  by_execution_mode: AcquisitionCostBreakdown[]
  worst_fills: AcquisitionFillDiagnostic[]
  rows: AcquisitionCostRow[]
}

export interface AcquisitionCostSnapshot {
  generated_at_us: number
  generation_duration_ms: number
  report: AcquisitionCostReport
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
export type ActualNavSeriesKey =
  | 'nav_change_before_fee_quote'
  | 'nav_change_after_fee_quote'
  | 'realized_pnl_before_fee_quote'
  | 'floating_pnl_quote'
  | 'estimated_trading_fee_quote'
export type TheoreticalNavSeriesKey =
  | 'theoretical_nav_before_fee_quote'
  | 'theoretical_nav_after_fee_quote'
export type NavSeriesKey = ActualNavSeriesKey | TheoreticalNavSeriesKey
export type SymbolRow = AggregateSymbolNavReport & {
  venues?: VenueNavReport[]
}
