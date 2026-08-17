import type { CatalogOrderStrategy, OrderParameters, PositionStrategy } from '../types'
import { DEFAULT_ORDER_STRATEGY_NAME } from './orderParametersMeta'

export const DEFAULT_ORDER: OrderParameters = {
  single_order_usdt: 100,
  orders_per_batch: 3,
  maker_price_anchor: 'own_best',
  tick_spacing: 1,
  batch_interval_ms: 500,
  maker_timeout_ms: 1000,
  max_maker_requotes: 2,
  target_tolerance_usdt: 10,
}

export function emptyPosition(): PositionStrategy {
  return { strategy_name: '', equity_usdt: 10_000, targets: {}, updated_at_us: 0 }
}

export function emptyOrder(): CatalogOrderStrategy {
  return {
    strategy_name: DEFAULT_ORDER_STRATEGY_NAME,
    order_parameters: { ...DEFAULT_ORDER },
    updated_at_us: 0,
  }
}

export function percent(ratio: number) {
  return `${(ratio * 100).toFixed(1)}%`
}

export function nextAllocationRatio(
  studio: { bindings: Array<{ binding_name: string; position_equity_usdt: number }>; bound_equity_usdt: number },
  bindingName: string,
  nextEquity: number,
) {
  const replaced =
    studio.bindings.find((binding) => binding.binding_name === bindingName)?.position_equity_usdt ?? 0
  const total = studio.bound_equity_usdt - replaced + nextEquity
  return total > 0 ? nextEquity / total : 0
}
