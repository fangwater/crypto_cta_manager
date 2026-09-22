import type { CatalogOrderStrategy, OrderParameters, PositionStrategy } from '../types'
import { DEFAULT_ORDER_STRATEGY_NAME } from './orderParametersMeta'

export const DEFAULT_ORDER: OrderParameters = {
  algorithm: 'batch',
  signal_execution_enabled: true,
  pov: {
    participation_rate: 0.1,
    max_batch_usdt: 300,
    max_carry_usdt: 600,
    volume_stale_ms: 5000,
    quote_stale_ms: 1000,
    duration_ms: 3600000,
    liquidity: 'maker_then_taker',
    limit_price: null,
  },
  chase: {
    batch_floor_usdt: 100,
    max_batch: 4,
    max_open_batches: 2,
    maker_recenter_trigger_bps: 5,
    maker_amend_cooldown_ms: 1000,
    maker_timeout_sec: 120,
    target_tolerance_usdt: 10,
    strategy_order_rate_limit_per_min: 0,
    strategy_order_rate_limit_10s: 0,
  },
  single_order_usdt: 100,
  orders_per_batch: 3,
  max_batch: 20,
  maker_price_anchor: 'own_best',
  tick_spacing: 1,
  batch_interval_ms: 500,
  maker_timeout_ms: 1000,
  max_maker_requotes: 2,
  target_tolerance_usdt: 10,
}

export function emptyPosition(): PositionStrategy {
  return { strategy_name: '', targets: {}, symbol_order_strategy_overrides: {}, updated_at_us: 0 }
}

export function emptyOrder(): CatalogOrderStrategy {
  return {
    strategy_name: DEFAULT_ORDER_STRATEGY_NAME,
    order_parameters: { ...DEFAULT_ORDER },
    updated_at_us: 0,
  }
}
