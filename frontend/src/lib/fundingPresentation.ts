import type { FeeMode, FundingEstimate, FundingSummary, FundingTimeline, StrategyFundingEstimate, StrategyNavTimeline } from '../types'

export function usesActualFunding(summary?: FundingSummary) {
  return summary?.enabled === true
}

/** Account collection selects factual income; strategies always use public estimates. */
export function displayedFunding(mode: 'account' | 'strategy', actual?: FundingTimeline, estimate?: FundingEstimate) {
  if (mode === 'account' && actual && usesActualFunding(actual.summary)) {
    return { kind: 'actual' as const, value: actual, income: actual.summary.income_quote }
  }
  if (estimate?.income_quote != null) {
    return { kind: 'estimated' as const, value: estimate, income: estimate.income_quote }
  }
  return { kind: 'unavailable' as const, value: undefined, income: null }
}

export function strategyFundingPoints(strategy: StrategyNavTimeline, estimate: StrategyFundingEstimate | undefined, feeMode: FeeMode, fundingOnly: boolean): number[][] {
  if (fundingOnly) {
    return estimate?.points.map((p) => [p.ts_us / 1_000, p.funding_income_quote]) ?? []
  }
  if (estimate) {
    return estimate.points.map((p) => [p.ts_us / 1_000, feeMode === 'after' ? p.total_nav_after_fee_quote : p.total_nav_before_fee_quote])
  }
  return strategy.points.map((p) => [p.ts_us / 1_000, feeMode === 'after' ? p.nav_change_after_fee_quote : p.nav_change_before_fee_quote])
}
