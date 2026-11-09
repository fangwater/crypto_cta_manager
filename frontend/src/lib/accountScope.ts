import type { DashboardAccount } from '../types'

/** Public identity stays on the account; native configuration selects a member. */
export function accountScope(accounts: DashboardAccount[], accountId: string, executionId?: string | null) {
  const account = accounts.find(a => a.source_id === accountId)
    ?? accounts.find(a => a.execution_sources?.some(s => s.source_id === accountId))
  if (!account) return null
  const members = account.execution_sources ?? []
  if (!members.length) {
    if (executionId && executionId !== account.source_id) throw new Error('执行腿不属于当前账户')
    return { account, execution: account }
  }
  const requested = executionId || (accountId !== account.source_id ? accountId : '')
  const execution = requested ? members.find(s => s.source_id === requested) : null
  if (requested && !execution) throw new Error('执行腿不属于当前账户')
  return { account, execution }
}
