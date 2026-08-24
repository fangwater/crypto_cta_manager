import type { CatalogOrderStrategy, OrderParameters, PositionStrategy } from '../types'
import { DEFAULT_ORDER_STRATEGY_NAME } from './orderParametersMeta'

export const DEFAULT_ORDER: OrderParameters = {
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
