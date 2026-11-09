import type { DashboardAccount } from '../types'

// One public account owns two execution sources. Never turn the sources into
// separate public accounts to make their Viz pages available.
export const executionGroups = [{
  id: 'cross01',
  accountId: 'cta_cross_spread_rx01',
  sourceIds: ['okex_exec_trade01', 'binance_exec_spread01'],
}] as const
export type ExecutionGroup = typeof executionGroups[number]

export function estimatedFundingOnly(sourceId: string) {
  return executionGroups.some(group => group.accountId === sourceId || (group.sourceIds as readonly string[]).includes(sourceId))
}

export function groupAccount(accounts: DashboardAccount[], group: ExecutionGroup) {
  const account = accounts.find(a => a.source_id === group.accountId)
  if (!account?.enabled || account.execution_sources?.length !== group.sourceIds.length) return null
  const selected = group.sourceIds.map(id => account.execution_sources!.find(a => a.source_id === id))
  if (new Set(group.sourceIds).size !== 2 || selected.some(a => !a?.enabled || !a.gateway_prefix || a.execution_sources?.length)) return null
  const result = selected as DashboardAccount[]
  if (result.some(a => !/^\/[a-zA-Z0-9_-]+$/.test(a.gateway_prefix!))) return null
  if (new Set(result.map(a => a.gateway_prefix)).size !== result.length) return null
  if (new Set(result.map(a => a.venue)).size !== 2 || !result.some(a => a.venue === 'okex-futures') || !result.some(a => a.venue === 'binance-futures')) return null
  return { account, executions: result }
}

export function accountExecutionGroup(account: DashboardAccount) {
  const group = executionGroups.find(g => g.accountId === account.source_id)
  return group && groupAccount([account], group) ? group : null
}

export function requireGroupSources(actual: string[], expected: readonly string[]) {
  if (actual.length !== 2 || new Set(actual).size !== 2 || expected.some(id => !actual.includes(id))) {
    throw new Error('返回的账户范围与所选执行组不一致')
  }
}
