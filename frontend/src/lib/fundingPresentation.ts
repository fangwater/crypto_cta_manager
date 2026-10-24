import type { FeeMode, FundingEstimate, FundingSummary, FundingTimeline, StrategyFundingEstimate, StrategyNavTimeline } from '../types'

export function usesActualFunding(summary?: FundingSummary) {
  return summary?.enabled === true
}

export function hasAvailableActualFunding(actual: FundingTimeline) {
  if (actual.summary.record_count > 0) return true
  const start = actual.points[0]?.ts_us
  const end = actual.points.at(-1)?.ts_us
  return start != null && end != null && actual.summary.symbols?.some((symbol) =>
    symbol.synced_through_ts_us != null && symbol.start_ts_us <= end && symbol.synced_through_ts_us >= start,
  ) === true
}

/** Partial public coverage may fall back to saved account income, including a
 * paused collector. Never add actual and estimated income for the same window,
 * and never assign an account statement to an individual strategy. */
export function displayedFunding(mode: 'account' | 'strategy', actual?: FundingTimeline, estimate?: FundingEstimate) {
  if (mode === 'account' && actual && hasAvailableActualFunding(actual)
    && (usesActualFunding(actual.summary) || estimate?.income_quote == null || Boolean(estimate.unavailable_reason))) {
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
