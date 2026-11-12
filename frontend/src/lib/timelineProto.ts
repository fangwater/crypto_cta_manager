import { fromBinary } from '@bufbuild/protobuf'
import { CurveKind, Metric, TimelineSchema } from '../generated/timeline_pb.js'
import type { Curve } from '../generated/timeline_pb.js'
import type { FundingNavPoint, NavTimelinePoint, TheoreticalNavPoint, TimelineOverlay, TimelineSnapshot } from '../types'

export type TimelinePayload = TimelineOverlay & Partial<Pick<TimelineSnapshot, 'report' | 'funding' | 'estimated_funding'>>

const metricNames: Partial<Record<Metric, string>> = {
  [Metric.REALIZED_BEFORE_FEE]: 'realized_pnl_before_fee_quote',
  [Metric.FLOATING_PNL]: 'floating_pnl_quote',
  [Metric.TRADING_FEE]: 'estimated_trading_fee_quote',
  [Metric.NAV_BEFORE_FEE]: 'nav_change_before_fee_quote',
  [Metric.NAV_AFTER_FEE]: 'nav_change_after_fee_quote',
  [Metric.GROSS_POSITION]: 'gross_position_value_quote',
  [Metric.NET_POSITION]: 'net_position_value_quote',
  [Metric.FUNDING_INCOME]: 'funding_income_quote',
  [Metric.TOTAL_NAV_BEFORE_FEE]: 'total_nav_before_fee_quote',
  [Metric.TOTAL_NAV_AFTER_FEE]: 'total_nav_after_fee_quote',
}
const actualMetrics = [Metric.REALIZED_BEFORE_FEE, Metric.FLOATING_PNL, Metric.TRADING_FEE,
  Metric.NAV_BEFORE_FEE, Metric.NAV_AFTER_FEE, Metric.GROSS_POSITION, Metric.NET_POSITION]
const fundingMetrics = [Metric.FUNDING_INCOME, Metric.TOTAL_NAV_BEFORE_FEE, Metric.TOTAL_NAV_AFTER_FEE]
const theoryMetrics = [Metric.NAV_BEFORE_FEE, Metric.NAV_AFTER_FEE, Metric.TRADING_FEE]

function safeTimestamp(value: bigint): number {
  const result = Number(value)
  if (!Number.isSafeInteger(result)) throw new Error('净值时间戳超出精确数值范围')
  return result
}

function points<T>(curve: Curve, axes: number[][], required: Metric[]): T[] {
  const times = axes[curve.axis]
  if (!times || times.length === 0) throw new Error('净值曲线缺少时间轴')
  const seen = new Set<Metric>()
  for (const column of curve.columns) {
    if (!metricNames[column.metric] || seen.has(column.metric)
      || column.values.length !== 1 && column.values.length !== times.length
      || column.values.some(value => !Number.isFinite(value))) throw new Error('净值数值列格式错误')
    seen.add(column.metric)
  }
  if (required.some(metric => !seen.has(metric))) throw new Error('净值曲线缺少数值列')
  return times.map((ts_us, i) => {
    const point: Record<string, number> = { ts_us }
    for (const column of curve.columns) point[metricNames[column.metric]!] = column.values[column.values.length === 1 ? 0 : i]
    return point as T
  })
}

/** Decode packed doubles without rounding, synthesizing zero trades, or changing NAV arithmetic. */
export function decodeTimeline(bytes: Uint8Array): TimelinePayload {
  const wire = fromBinary(TimelineSchema, bytes)
  if (!wire.metadata) throw new Error('净值响应缺少元数据')
  const result = wire.metadata as unknown as TimelinePayload
  if (!Array.isArray(result.theoretical) || !Array.isArray(result.execution_slippage)) throw new Error('净值响应格式错误')
  const axes = wire.axes.map(axis => {
    let timestamp = axis.startUs
    const times = [safeTimestamp(timestamp)]
    for (const delta of axis.deltasUs) {
      if (delta < 0n) throw new Error('净值时间轴顺序错误')
      timestamp += delta
      times.push(safeTimestamp(timestamp))
    }
    return times
  })
  const seen = new Set<string>()
  for (const curve of wire.curves) {
    const identity = JSON.stringify([curve.kind, curve.key])
    if (seen.has(identity)) throw new Error('净值响应包含重复曲线')
    seen.add(identity)
    switch (curve.kind) {
      case CurveKind.PORTFOLIO:
        if (!result.report) throw new Error('净值响应缺少报表')
        result.report.points = points<NavTimelinePoint>(curve, axes, actualMetrics)
        break
      case CurveKind.SYMBOL:
        if (!result.report) throw new Error('净值响应缺少报表')
        result.report.symbol_points.push({ symbol: curve.key, points: points<NavTimelinePoint>(curve, axes, actualMetrics) })
        break
      case CurveKind.STRATEGY: {
        const row = result.report?.strategy_points.find(row => row.strategy === curve.key)
        if (!row) throw new Error('净值响应缺少策略汇总')
        row.points = points<NavTimelinePoint>(curve, axes, actualMetrics)
        break
      }
      case CurveKind.ACTUAL_FUNDING:
        if (!result.funding) throw new Error('净值响应缺少账户资金费汇总')
        result.funding.points = points<FundingNavPoint>(curve, axes, fundingMetrics)
        break
      case CurveKind.ESTIMATED_FUNDING:
        if (!result.estimated_funding) throw new Error('净值响应缺少估算资金费汇总')
        result.estimated_funding.points = points<FundingNavPoint>(curve, axes, fundingMetrics)
        break
      case CurveKind.STRATEGY_FUNDING: {
        const row = result.estimated_funding?.strategies.find(row => row.strategy === curve.key)
        if (!row) throw new Error('净值响应缺少策略资金费汇总')
        row.points = points<FundingNavPoint>(curve, axes, fundingMetrics)
        break
      }
      case CurveKind.THEORETICAL: {
        const index = Number(curve.key)
        const row = Number.isSafeInteger(index) && index >= 0 ? result.theoretical[index] : undefined
        if (!row) throw new Error('净值响应缺少理论曲线说明')
        row.points = points<TheoreticalNavPoint>(curve, axes, theoryMetrics)
        break
      }
      default: throw new Error('净值响应包含未知曲线')
    }
  }
  return result
}
