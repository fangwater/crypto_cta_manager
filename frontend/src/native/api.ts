import { requestJson } from '../api'
export interface Source { source_id: string; account: string; adapter: string; access_level: 'view' | 'configure'; execution_enabled: false }
export interface Position { venue: string; symbol: string; side: 'buy' | 'sell'; position: { today: number; yesterday: number; frozen_today: number; frozen_yesterday: number } }
export interface Order { phase: string; exchange_terminal: boolean; facts_complete: boolean; record: {
  identity: { trading_day: string; venue: string; native: { session: string; order_ref: string } };
  order: { request: { symbol: string; side: string; offset: string; lots: number; price_type: string; time_in_force: string }; status: string; filled: number; reported_filled: number; cancelled: number; cancel_pending: boolean; system_id: string; limit_price: number };
  send_state: string; updated_us: number; locally_submitted: boolean;
} }
export interface Fill { fill: { identity: { venue: string; symbol: string; trading_day: string; trade_id: string; side: string }; system_id: string; order_ref: string; lots: number; price: number; offset: string }; commission: number | null; commission_currency: string | null; native_time: number | null; received_us: number }
export interface ReplicaStatus {
  initialized: boolean; applying: boolean; committed_cursor: number; received_cursor: number;
  target_cursor: number | null; source_captured_at_us: number | null; target_captured_at_us: number | null;
  last_synced_at_us: number | null; last_attempt_at_us: number | null; last_error: string | null;
}
export interface Snapshot {
  replication: ReplicaStatus | null;
  replication_stale: boolean;
  source_id: string; account: string; read_at_us: number; last_event_us: number | null; state_observed_us: number | null;
  nav_anchor_observed_us: number | null; nav_anchor_evidence_sequence: number | null;
  status: string; snapshot_ready: boolean; snapshot_current_runtime: boolean; transport_observed_us: number | null; reported_can_trade: boolean | null; transport_ready: boolean | null;
  reason: string; trading_day: string | null; available_local_budget: number | null; positions_may_lag: boolean;
  positions: Position[]; orders: Order[]; fills: Fill[]; total_orders: number; total_fills: number; projection_errors: number;
}
export interface Strategy { strategy: { strategy_name: string; targets: Record<string, { qty: number | 'clear'; signal: number }>; updated_at_us: number }; can_configure: boolean }
export interface Grant { user_id: number; username?: string; access_level: string }
export interface StrategyGrants { strategy_name: string; created_by: string | null; open_visibility: boolean; grants: Grant[]; managers: { user_id: number; username: string }[] }
export const getSources = (signal?: AbortSignal) => requestJson<Source[]>('/native/sources', { signal })
export const getSnapshot = (id: string, signal?: AbortSignal) => requestJson<Snapshot>(`/native/sources/${encodeURIComponent(id)}/snapshot`, { signal })
export interface CostSignal {
  signal_id: string; venue: string; symbol: string; side: string; offset: string;
  expected_lots: number; priced_lots: number; model: 'twap' | 'vwap'; window_minutes: number;
  sample_open_us: number[]; sample_prices: number[]; sample_weights: number[];
  theoretical_price: number | null; reason: string | null;
}
export interface CostFill {
  venue: string; symbol: string; trading_day: string; trade_id: string;
  session: string | null; order_ref: string | null; side: string; offset: string; lots: number;
  actual_price: number; signal_id: string | null; theoretical_price: number | null;
  actual_commission: number | null; commission_currency: string | null;
  theoretical_commission_cny: number | null; price_shortfall_cny: number | null;
  fee_shortfall_cny: number | null; total_shortfall_cny: number | null; reason: string | null;
}
export interface CostReport {
  replication: ReplicaStatus | null;
  source_id: string; trading_day: string; generated_at_us: number; reference_available: boolean;
  reference_sha256: string | null; provenance: string | null; reason: string | null;
  market_data_error: string | null;
  complete: boolean; fill_count: number; priced_fill_count: number; fee_compared_fill_count: number;
  total_lots: number; priced_lots: number; price_shortfall_cny: number | null;
  fee_shortfall_cny: number | null; total_shortfall_cny: number | null;
  covered_price_shortfall_cny: number | null; covered_fee_shortfall_cny: number | null;
  signals: CostSignal[]; fills: CostFill[];
}
export const getCostReport = (id: string, day: string, signal?: AbortSignal) => requestJson<CostReport>(`/native/sources/${encodeURIComponent(id)}/cost?trading_day=${encodeURIComponent(day)}`, { signal })
export const getStrategies = (signal?: AbortSignal) => requestJson<Strategy[]>('/native/strategies', { signal })
export const saveStrategy = (name: string, targets: Strategy['strategy']['targets']) => requestJson('/native/strategies', { method: 'POST', body: { strategy_name: name, targets } })
export const getGrants = (id: string) => requestJson<Grant[]>(`/native/sources/${encodeURIComponent(id)}/grants`)
export const saveGrants = (id: string, grants: Grant[]) => requestJson<Grant[]>(`/native/sources/${encodeURIComponent(id)}/grants`, { method: 'PUT', body: { grants } })
export const getStrategyGrants = (name: string) => requestJson<StrategyGrants>(`/native/strategies/${encodeURIComponent(name)}/grants`)
export const saveStrategyGrants = (name: string, grants: Grant[], open_visibility: boolean) => requestJson<StrategyGrants>(`/native/strategies/${encodeURIComponent(name)}/grants`, { method: 'PUT', body: { grants, open_visibility } })

export interface NavAnchor {
  source_id: string; snapshot_us: number; trading_day: string; evidence_sequence: number;
  evidence_sha256: string; store_id: string; note: string;
  positions: { venue: string; symbol: string; side: 'buy' | 'sell'; today: number; yesterday: number; reference_price: number; contract_multiplier: number }[];
}
export interface NavPoint {
  timestamp_us: number; nav_before_fees_cny: number; realized_cny: number; floating_cny: number;
  fees_cny: number | null; nav_after_fees_cny: number | null;
}
export interface NavReport {
  source_id: string; ready: boolean; reason: string | null; currency: 'CNY'; valuation: string;
  snapshot_us: number | null; start_us: number; end_us: number; source_through_us: number | null;
  fees: { fill_count: number; actual_count: number; estimated_count: number; missing_count: number; actual_known_cny: number; estimated_known_cny: number };
  summary: NavPoint | null; points: NavPoint[];
  markets: { venue: string; symbol: string; long_lots: number; short_lots: number; realized_cny: number; floating_cny: number; nav_before_fees_cny: number }[];
}
export const getNavAnchors = (id: string, signal?: AbortSignal) => requestJson<NavAnchor[]>(`/native/sources/${encodeURIComponent(id)}/nav-snapshots`, { signal })
export const captureNavAnchor = (id: string, expectedState: number, note: string) => requestJson<NavAnchor>(`/native/sources/${encodeURIComponent(id)}/nav-snapshots`, { method: 'POST', body: { expected_state_us: expectedState, note } })
export const getNativeNav = (id: string, start: number, end: number, signal?: AbortSignal) => requestJson<NavReport>(`/native/sources/${encodeURIComponent(id)}/nav?start_ms=${start}&end_ms=${end}&max_points=1000`, { signal })
