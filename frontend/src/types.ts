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
  quote_fee_rates: Record<string, QuoteFeeRates>
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
  target_proxy_enabled: boolean
  /** Current session's grant on this account. Undefined only in stale payloads. */
  access_level?: 'view' | 'configure' | null
  account_pnl_start_ts_us?: number | null
  strategy_pnl_start_ts_us?: number | null
  live_equity_usdt?: number | null
  live_equity_status?: 'ok' | 'stale' | string | null
  /** Account monitor has published an OKX unified AccountRisk sample. */
  unified_account?: boolean
  /** OKX maintenance margin ratio. Higher is safer. */
  uni_mmr?: number | null
  uni_mmr_status?: 'ok' | 'stale' | string | null
  funding?: FundingSummary
  total_nav_before_fee_quote?: number | null
  total_nav_after_fee_quote?: number | null
}

export interface OrderParameters {
  algorithm: 'batch' | 'pov' | 'chase' | 'ctaker' | 'grid'
  pov: PovParameters
  grid: GridParameters
  chase: ChaseParameters
  signal_execution_enabled: boolean
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

export interface GridParameters {
  spot: GridMarketParameters
  futures: GridMarketParameters
}

export interface GridMarketParameters {
  orders_per_batch: number
  maker_timeout_ms: number
  vol_multiplier: number
  /** Additional rounds after the first; null means unlimited. */
  max_maker_requotes: number | null
}

export interface PovParameters {
  participation_rate: number
  max_batch_usdt: number
  max_carry_usdt: number
  volume_stale_ms: number
  quote_stale_ms: number
  duration_ms: number
  liquidity: 'maker_only' | 'taker_only' | 'maker_then_taker'
  limit_price: number | null
}

export interface ChaseParameters {
  batch_floor_usdt: number
  max_batch: number
  max_open_batches: number
  maker_recenter_trigger_bps: number
  maker_amend_cooldown_ms: number
  maker_timeout_sec: number
  target_tolerance_usdt: number
  strategy_order_rate_limit_per_min: number
  strategy_order_rate_limit_10s: number
}

export interface OrderParameterOverrides {
  algorithm?: 'batch' | 'pov' | 'ctaker' | 'grid'
  pov?: PovParameters
  grid?: GridParameters
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
  symbol_overrides: Record<string, OrderParameterOverrides | Partial<ChaseParameters>>
  updated_at_us: number | null
  target_count: number
  nonzero_target_count: number
}

export interface OrderStrategyList {
  source_id: string
  strategies: string[]
}

export interface PositionStrategy {
  /** Session-specific permission returned by the strategy catalog. */
  can_publish?: boolean
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
  owner?: string | null
  can_configure?: boolean
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

export interface BindingTargetPreview {
  source_id: string
  binding_name: string
  shares: number
  proxy_enabled: boolean
  targets: Record<string, TargetPosition>
  projection: {
    rules_fetched_at_us: number
    matched: Record<string, {
      symbol: string
      source_contract_multiplier: number
      target_contract_multiplier: number
      quantity_multiplier: number
    }>
    filtered: Record<string, string>
  } | null
}

export interface AccountStudio {
  source_id: string
  /** NAV estimated trading fee rate as a fraction (e.g. 0.0004 = 4 bps). */
  estimated_fee_rate: number
  maker_fee_rate: number
  taker_fee_rate: number
  quote_fee_rates: Record<string, QuoteFeeRates>
  theoretical_twap_fee_rate: number
  bindings: AccountBinding[]
  configuration: AccountConfiguration
  pending_publishes: { binding_name: string; error: string | null }[]
}

export interface ExecOrderRateLimits {
  source_id: string
  exec_order_rate_limit_per_min: number
  exec_order_rate_limit_10s: number
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
  theoretical: TheoreticalNavTimeline[]
  execution_slippage: ExecutionSlippage[]
  funding?: FundingTimeline
  estimated_funding?: FundingEstimate
}

export interface ExecutionSlippage {
  start_ts_us: number
  end_ts_us: number | null
  volume_quote: number
  before_fee_gap_quote: number | null
  after_fee_gap_quote: number | null
  before_fee_bps: number | null
  after_fee_bps: number | null
  unavailable_reason: string | null
}

export interface FundingSummary {
  enabled: boolean
  refresh_secs: number
  status: string
  start_ts_us: number | null
  synced_through_ts_us: number | null
  last_success_ts_us: number | null
  last_error: string | null
  record_count: number
  income_quote: number
  income_by_asset: Record<string, number>
  symbols: { source_id: string; symbol: string; start_ts_us: number; synced_through_ts_us: number | null }[]
}

export interface FundingSettings {
  source_id: string
  enabled: boolean
  refresh_secs: number
  local_ip: string | null
  public_ip: string | null
  verified_at_us: number | null
  verified_local_ip: string | null
  verified_public_ip: string | null
  unavailable_reason: string | null
  summary: FundingSummary
}

export interface FundingNavPoint {
  ts_us: number
  funding_income_quote: number
  total_nav_before_fee_quote: number
  total_nav_after_fee_quote: number
}

export interface FundingTimeline {
  summary: FundingSummary
  total_nav_before_fee_quote: number
  total_nav_after_fee_quote: number
  points: FundingNavPoint[]
  records: { source_id: string; venue: string; symbol: string; asset: string; ts_us: number; income_quote: number }[]
  records_sampled: boolean
}

export interface TheoreticalNavPoint {
  ts_us: number
  nav_change_before_fee_quote: number
  nav_change_after_fee_quote: number
  estimated_trading_fee_quote: number
}

export type TheoreticalExecutionModel = 'twap5m' | 'vwap3m' | 'twap3m' | 'instant_taker'

export interface TheoreticalNavTimeline {
  execution_model: TheoreticalExecutionModel | null
  loading: boolean
  unavailable_reason: string | null
  missing_price_count: number
  legacy_fee_delta_count: number
  zero_volume_fallback_sample_count: number
  zero_volume_skipped_sample_count: number
  valuation: string
  execution_window_secs: number
  price_basis: string
  fee_basis: string
  available_from_us: number | null
  latest_point_ts_us: number | null
  points: TheoreticalNavPoint[]
  sampled: boolean
}

export interface AcquisitionCostTotals {
  virtual_delta_count: number
  missing_virtual_delta_count: number
  pending_virtual_delta_count: number
  zero_volume_fallback_delta_count: number
  zero_volume_skipped_delta_count: number
  legacy_fee_delta_count: number
  zero_volume_fallback_sample_count: number
  zero_volume_skipped_sample_count: number
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
  first_minute_shortfall_usdt: number
  five_sample_drift_usdt: number
  fee_shortfall_usdt: number
  after_fee_shortfall_usdt: number
  price_shortfall_bps: number
  after_fee_shortfall_bps: number
  first_minute_shortfall_bps: number
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
  first_minute_shortfall_usdt: number
  five_sample_drift_usdt: number
  price_shortfall_bps: number
  after_fee_shortfall_bps: number
  first_minute_shortfall_bps: number
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
  first_minute_shortfall_usdt: number
  five_sample_drift_usdt: number
  price_shortfall_bps: number
}

export interface AcquisitionExecutionFill {
  fill_ts_us: number
  actual_price: number
  actual_qty: number
  liquidity: string
  target_delay_us: number
  order_delay_us: number
}

export interface AcquisitionExecution {
  first_fill_ts_us: number | null
  last_fill_ts_us: number | null
  qty_weighted_target_delay_us: number | null
  qty_weighted_order_delay_us: number | null
  after_window_qty_ratio: number | null
  fills_sampled: boolean
  fills: AcquisitionExecutionFill[]
}

export interface AcquisitionCostRow {
  reference_prices: { execution_model: TheoreticalExecutionModel; price: number | null }[]
  execution_model: TheoreticalExecutionModel
  execution_window_secs: number
  source_id: string
  binding_name: string
  strategy_name: string
  symbol: string
  venue: string
  received_at_us: number
  virtual_execution_ts_us: number
  delta_qty: number
  sample_prices: (number | null)[]
  sample_quantity_weights: number[]
  zero_volume_fallback_sample_count: number
  zero_volume_skipped_sample_count: number
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
  after_fee_shortfall_bps: number | null
  execution: AcquisitionExecution
}

export interface AcquisitionCostReport {
  execution_model: TheoreticalExecutionModel | null
  execution_window_secs: number
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
  row_symbol: string | null
  row_strategy: string | null
  row_sort: 'latest' | 'price_shortfall'
  filtered_row_count: number
  totals: AcquisitionCostTotals
  points: AcquisitionCostPoint[]
  warnings: string[]
  by_strategy: AcquisitionCostBreakdown[]
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
  reports: AcquisitionCostReport[]
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
export type TimelineChartMode = 'portfolio' | 'symbols' | 'strategies' | 'strategyFunding'
export type ActualNavSeriesKey =
  | 'nav_change_before_fee_quote'
  | 'nav_change_after_fee_quote'
  | 'realized_pnl_before_fee_quote'
  | 'floating_pnl_quote'
  | 'estimated_trading_fee_quote'
export type TheoreticalNavSeriesKey =
  | 'theoretical_nav_before_fee_quote'
  | 'theoretical_nav_after_fee_quote'
  | 'theoretical_twap3m_before_fee_quote'
  | 'theoretical_twap3m_after_fee_quote'
  | 'theoretical_instant_taker_before_fee_quote'
  | 'theoretical_instant_taker_after_fee_quote'
export type FundingNavSeriesKey = 'funding_income_quote' | 'total_nav_before_fee_quote' | 'total_nav_after_fee_quote'
export type NavSeriesKey = ActualNavSeriesKey | TheoreticalNavSeriesKey | FundingNavSeriesKey
export type SymbolRow = AggregateSymbolNavReport & {
  venues?: VenueNavReport[]
}

export interface VirtualBinding {
  binding_name: string
  position_strategy_name: string
  order_strategy_name: string
  shares: number
}

export interface VirtualAccount {
  virtual_id: string
  name: string
  bindings: VirtualBinding[]
  updated_at_us: number
  created_by_user_id: number | null
  owner_username: string | null
  managers: { user_id: number; username: string }[]
  can_configure: boolean
  can_manage_grants: boolean
  followers: {
    source_id: string
    multiplier: number
    pending_publishes: { binding_name: string; error: string | null }[]
  }[]
}

export type AccountConfiguration = { mode: 'independent' } | {
  mode: 'follow'
  virtual_id: string
  multiplier: number
}

export interface StrategyFundingEstimate {
  strategy: string
  income_quote: number
  income_by_asset: Record<string, number>
  total_nav_before_fee_quote: number
  total_nav_after_fee_quote: number
  points: FundingNavPoint[]
}

export interface FundingEstimate {
  reconciliation: {
    compared_settlements: number
    actual_record_count: number
    unobserved_settlements: number
    unmatched_actual_records: number
    estimated_income_quote: number
    actual_income_quote: number
    difference_quote: number
    absolute_error_quote: number
    max_error_quote: number
    mismatch_count: number
  } | null
  loading: boolean
  unavailable_reason: string | null
  synced_through_ts_us: number | null
  income_quote: number | null
  income_by_asset: Record<string, number>
  total_nav_before_fee_quote: number | null
  total_nav_after_fee_quote: number | null
  settlement_count: number
  boundary_fill_count: number
  points: FundingNavPoint[]
  strategies: StrategyFundingEstimate[]
}

export interface QuoteFeeRates {
  maker_fee_rate: number
  taker_fee_rate: number
  theoretical_twap_fee_rate: number
}
