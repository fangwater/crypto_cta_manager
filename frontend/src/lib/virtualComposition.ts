import type { VirtualAccount, VirtualBinding, VirtualChild } from '../types'

export function canAddVirtualChild(accounts: VirtualAccount[], parent: string, candidate: string) {
  const byId = new Map(accounts.map(account => [account.virtual_id, account]))
  const seen = new Set<string>()
  const reachesParent = (id: string): boolean => {
    if (parent && id === parent) return true
    if (seen.has(id)) return false
    seen.add(id)
    return byId.get(id)?.children.some(child => reachesParent(child.virtual_id)) ?? false
  }
  return byId.has(candidate) && !reachesParent(candidate)
}

/** Per-one-parent preview only; the server validates and publishes the full graph. */
export function composeVirtual(
  bindings: VirtualBinding[], children: VirtualChild[], accounts: VirtualAccount[],
): VirtualBinding[] {
  const combined = new Map<string, VirtualBinding>()
  const valid = (value: number) => Number.isFinite(value) && value >= 0
  const add = (binding: VirtualBinding, multiplier: number) => {
    const shares = binding.shares * multiplier
    if (!valid(binding.shares) || !valid(multiplier) || !valid(shares)) throw new Error('份数与配比必须为有限的非负数')
    const existing = combined.get(binding.binding_name)
    if (existing) {
      if (existing.position_strategy_name !== binding.position_strategy_name
        || existing.order_strategy_name !== binding.order_strategy_name) throw new Error('发布名 ' + binding.binding_name + ' 的仓位策略或下单模板冲突')
      if (!valid(existing.shares + shares)) throw new Error('合并后的份数超出有效范围')
      existing.shares += shares
    } else combined.set(binding.binding_name, { ...binding, shares })
  }
  const names = new Set<string>()
  for (const binding of bindings) {
    if (names.has(binding.binding_name)) throw new Error('发布名重复：' + binding.binding_name)
    names.add(binding.binding_name)
    add(binding, 1)
  }
  const ids = new Set<string>()
  for (const child of [...children].sort((a, b) => a.virtual_id < b.virtual_id ? -1 : a.virtual_id > b.virtual_id ? 1 : 0)) {
    if (ids.has(child.virtual_id)) throw new Error('子 Virtual 重复：' + child.virtual_id)
    ids.add(child.virtual_id)
    const account = accounts.find(account => account.virtual_id === child.virtual_id)
    if (!account) throw new Error('子 Virtual 不可见或已删除：' + child.virtual_id)
    if (!valid(child.multiplier)) throw new Error('子 Virtual 配比必须为有限的非负数')
    for (const binding of account.expanded_bindings) add(binding, child.multiplier)
  }
  return [...combined.values()].sort((a, b) => a.binding_name.localeCompare(b.binding_name))
}
