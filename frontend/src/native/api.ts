import { requestJson } from '../api'
export interface Source { source_id: string; account: string; adapter: string; access_level: 'view' | 'configure'; execution_enabled: false }
export interface Position { venue: string; symbol: string; side: 'buy' | 'sell'; position: { today: number; yesterday: number; frozen_today: number; frozen_yesterday: number } }
export interface Order { phase: string; exchange_terminal: boolean; facts_complete: boolean; record: {
  identity: { trading_day: string; venue: string; native: { session: string; order_ref: string } };
  order: { request: { symbol: string; side: string; offset: string; lots: number; price_type: string; time_in_force: string }; status: string; filled: number; reported_filled: number; cancelled: number; cancel_pending: boolean; system_id: string; limit_price: number };
  send_state: string; updated_us: number; locally_submitted: boolean;
} }
export interface Fill { fill: { identity: { venue: string; symbol: string; trading_day: string; trade_id: string; side: string }; system_id: string; order_ref: string; lots: number; price: number; offset: string }; commission: number | null; commission_currency: string | null; native_time: number | null; received_us: number }
export interface Snapshot {
  source_id: string; account: string; read_at_us: number; last_event_us: number | null; state_observed_us: number | null;
  status: string; snapshot_ready: boolean; snapshot_current_runtime: boolean; transport_observed_us: number | null; reported_can_trade: boolean | null; transport_ready: boolean | null;
  reason: string; trading_day: string | null; available_local_budget: number | null; positions_may_lag: boolean;
  positions: Position[]; orders: Order[]; fills: Fill[]; total_orders: number; total_fills: number; projection_errors: number;
}
export interface Strategy { strategy: { strategy_name: string; targets: Record<string, { qty: number | 'clear'; signal: number }>; updated_at_us: number }; can_configure: boolean }
export interface Grant { user_id: number; username?: string; access_level: string }
export interface StrategyGrants { strategy_name: string; created_by: string | null; open_visibility: boolean; grants: Grant[]; managers: { user_id: number; username: string }[] }
export const getSources = (signal?: AbortSignal) => requestJson<Source[]>('/native/sources', { signal })
export const getSnapshot = (id: string, signal?: AbortSignal) => requestJson<Snapshot>(`/native/sources/${encodeURIComponent(id)}/snapshot`, { signal })
export const getStrategies = (signal?: AbortSignal) => requestJson<Strategy[]>('/native/strategies', { signal })
export const saveStrategy = (name: string, targets: Strategy['strategy']['targets']) => requestJson('/native/strategies', { method: 'POST', body: { strategy_name: name, targets } })
export const getGrants = (id: string) => requestJson<Grant[]>(`/native/sources/${encodeURIComponent(id)}/grants`)
export const saveGrants = (id: string, grants: Grant[]) => requestJson<Grant[]>(`/native/sources/${encodeURIComponent(id)}/grants`, { method: 'PUT', body: { grants } })
export const getStrategyGrants = (name: string) => requestJson<StrategyGrants>(`/native/strategies/${encodeURIComponent(name)}/grants`)
export const saveStrategyGrants = (name: string, grants: Grant[], open_visibility: boolean) => requestJson<StrategyGrants>(`/native/strategies/${encodeURIComponent(name)}/grants`, { method: 'PUT', body: { grants, open_visibility } })
