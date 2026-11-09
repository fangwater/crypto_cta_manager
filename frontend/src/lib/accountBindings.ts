import type { AccountBinding, AccountStudio } from '../types'

/** One account strategy may have settings in several markets. Never add shares. */
export function accountBindings(studios: AccountStudio[]) {
  const grouped = new Map<string, { binding_name: string; position_strategy_name: string; markets: AccountBinding[] }>()
  for (const studio of studios) for (const binding of studio.bindings) {
    const key = JSON.stringify([binding.binding_name, binding.position_strategy_name])
    const entry = grouped.get(key) ?? { binding_name: binding.binding_name, position_strategy_name: binding.position_strategy_name, markets: [] }
    entry.markets.push(binding)
    grouped.set(key, entry)
  }
  return [...grouped.values()].map(entry => ({
    ...entry,
    order_strategy_name: entry.markets.every(b => b.order_strategy_name === entry.markets[0].order_strategy_name) ? entry.markets[0].order_strategy_name : '按市场配置',
    shares: entry.markets.every(b => b.shares === entry.markets[0].shares) ? entry.markets[0].shares : null,
  }))
}
