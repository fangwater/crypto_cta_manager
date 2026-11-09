import type { StrategyFundingEstimate, StrategyNavTimeline, TimelineSnapshot } from '../types'

function hasNonzeroValues(values: Record<string, number>, ignoredKey?: string) {
  return Object.entries(values).some(([key, value]) => key !== ignoredKey && value !== 0)
}

/** Hide only empty window series. Historical fills, carried exposure and even
 * offsetting or very small funding must keep their original strategy identity. */
export function hasNavStrategyActivity(strategy: StrategyNavTimeline, funding?: StrategyFundingEstimate) {
  return strategy.symbol_count !== 0
    || strategy.gross_position_value_quote !== 0
    || strategy.net_position_value_quote !== 0
    || hasNonzeroValues({ ...strategy.summary })
    || strategy.points.some(point => hasNonzeroValues({ ...point }, 'ts_us'))
    || (funding !== undefined && (
      funding.income_quote !== 0
      || hasNonzeroValues(funding.income_by_asset)
      || funding.points.some(point => hasNonzeroValues({ ...point }, 'ts_us'))
    ))
}

export function activeNavStrategies(timeline: TimelineSnapshot | null): string[] {
  if (!timeline) return []
  const funding = new Map(timeline.estimated_funding?.strategies.map(row => [row.strategy, row]))
  const strategies = new Map(timeline.report.strategy_points.map(row => [row.strategy, row]))
  return timeline.report.available_strategies.filter(name => {
    const strategy = strategies.get(name)
    // An incomplete response is not evidence that a strategy is empty.
    return !strategy || hasNavStrategyActivity(strategy, funding.get(name))
  })
}
