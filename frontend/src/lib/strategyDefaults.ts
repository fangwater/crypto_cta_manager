import type { CatalogOrderStrategy, OrderParameters, PositionStrategy } from '../types'
import { DEFAULT_ORDER_STRATEGY_NAME } from './orderParametersMeta'

export const DEFAULT_ORDER: OrderParameters = {
  algorithm: 'batch',
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
    single_order_usdt: 100,
    max_open_usdt: 200,
    maker_recenter_trigger_bps: 3,
    maker_amend_cooldown_ms: 0,
    maker_timeout_ms: 60000,
    target_tolerance_usdt: 10,
    bbo_max_age_ms: 2000,
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
