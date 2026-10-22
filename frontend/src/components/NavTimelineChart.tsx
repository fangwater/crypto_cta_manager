import { LineChart } from 'echarts/charts'
import {
  DataZoomComponent,
  GridComponent,
  MarkLineComponent,
  TooltipComponent,
} from 'echarts/components'
import * as echarts from 'echarts/core'
import { CanvasRenderer } from 'echarts/renderers'
import { useEffect, useRef } from 'react'
import type {
  ActualNavSeriesKey,
  FeeMode,
  FundingNavPoint,
  FundingNavSeriesKey,
  NavSeriesKey,
  NavTimelinePoint,
  StrategyNavTimeline,
  StrategyFundingEstimate,
  SymbolNavTimeline,
  TheoreticalNavTimeline,
  TheoreticalExecutionModel,
  TimelineChartMode,
} from '../types'
import { fundingMoney, strategyLabel, UI_FONT_SANS } from '../format'
import { strategyFundingPoints } from '../lib/fundingPresentation'

echarts.use([
  LineChart,
  DataZoomComponent,
  GridComponent,
  MarkLineComponent,
  TooltipComponent,
  CanvasRenderer,
])

interface Props {
  points: NavTimelinePoint[]
  theoretical: TheoreticalNavTimeline[]
  fundingPoints?: FundingNavPoint[]
  fundingIsEstimate?: boolean
  strategyFunding?: StrategyFundingEstimate[]
  symbolPoints: SymbolNavTimeline[]
  strategyPoints: StrategyNavTimeline[]
  visibleSeries: NavSeriesKey[]
  mode: TimelineChartMode
  feeMode: FeeMode
}

const symbolPalette = [
  '#176b5b',
  '#2563a7',
  '#b7791f',
  '#c2413b',
  '#7357a3',
  '#2f855a',
  '#9c4f87',
  '#4b6478',
  '#d97706',
  '#0f766e',
  '#4467a8',
  '#a33f55',
]

export const navSeriesMeta: Record<
  NavSeriesKey,
  { label: string; color: string; lineType?: 'dashed' | 'dotted' }
> = {
  nav_change_before_fee_quote: { label: '费前净值', color: '#2563a7' },
  nav_change_after_fee_quote: { label: '费后净值', color: '#176b5b' },
  funding_income_quote: { label: 'Funding（累计资金费）', color: '#d97706' },
  total_nav_before_fee_quote: { label: '费前总体净值（含 funding）', color: '#0891b2' },
  total_nav_after_fee_quote: { label: '费后总体净值（含 funding）', color: '#15803d' },
  theoretical_nav_before_fee_quote: {
    label: '理论费前（TWAP，分钟 VWAP）',
    color: '#7357a3',
    lineType: 'dotted',
  },
  theoretical_nav_after_fee_quote: {
    label: '理论费后（TWAP，分钟 VWAP）',
    color: '#a33f55',
    lineType: 'dashed',
  },
  theoretical_twap3m_before_fee_quote: { label: '理论3min_twap（费前）', color: '#0891b2', lineType: 'dotted' },
  theoretical_twap3m_after_fee_quote: { label: '理论3min_twap（费后）', color: '#d97706', lineType: 'dashed' },
  realized_pnl_before_fee_quote: { label: '已实现盈亏', color: '#b7791f' },
  floating_pnl_quote: {
    label: '浮动盈亏',
    color: '#4b6478',
    lineType: 'dashed',
  },
  estimated_trading_fee_quote: {
    label: '估算手续费',
    color: '#c2413b',
    lineType: 'dashed',
  },
}

export function navSeriesLabel(key: NavSeriesKey, model?: TheoreticalExecutionModel | null) {
  if (key.startsWith('theoretical_')) {
    const fee = key.endsWith('before_fee_quote') ? '费前' : '费后'
    if (key.startsWith('theoretical_twap3m_')) return `理论3min_twap（${fee}）`
    if (model === 'vwap3m') return `理论3min_vwap（${fee}）`
    if (model === 'twap5m') return `理论${fee}（5 分钟 TWAP）`
    return `理论${fee}`
  }
  return navSeriesMeta[key].label
}

function isTheoreticalSeries(key: NavSeriesKey) {
  return key.startsWith('theoretical_')
}

function money(value: number) {
  return value.toLocaleString('en-US', {
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  })
}

function chartTime(value: number) {
  const date = new Date(value)
  const month = String(date.getMonth() + 1).padStart(2, '0')
  const day = String(date.getDate()).padStart(2, '0')
  const hour = String(date.getHours()).padStart(2, '0')
  const minute = String(date.getMinutes()).padStart(2, '0')
  return `${month}-${day}\n${hour}:${minute}`
}

export function NavTimelineChart({
  points,
  theoretical,
  fundingPoints,
  fundingIsEstimate,
  strategyFunding,
  symbolPoints,
  strategyPoints,
  visibleSeries,
  mode,
  feeMode,
}: Props) {
  const containerRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (!containerRef.current) return

    const chart = echarts.init(containerRef.current, undefined, {
      renderer: 'canvas',
    })
    const fundingOnly = mode === 'strategyFunding'
    const series =
      mode === 'portfolio'
        ? visibleSeries.map((key) => {
            const meta = { ...navSeriesMeta[key], label: navSeriesLabel(key, theoretical[0]?.execution_model) }
            const isTheoretical = isTheoreticalSeries(key)
            const reference = key.startsWith('theoretical_twap3m_') ? theoretical[1] : theoretical[0]
            const data = isTheoretical
              ? (reference?.points ?? []).map((point) => [
                  point.ts_us / 1_000,
                  key.endsWith('after_fee_quote')
                    ? point.nav_change_after_fee_quote
                    : point.nav_change_before_fee_quote,
                ])
              : key === 'funding_income_quote' || key.startsWith('total_nav_')
                ? (fundingPoints ?? []).map((point) => [point.ts_us / 1_000, point[key as FundingNavSeriesKey]])
                : points.map((point) => [
                  point.ts_us / 1_000,
                  point[key as ActualNavSeriesKey],
                ])
            return {
              name: fundingIsEstimate && (key === 'funding_income_quote' || key.startsWith('total_nav_')) ? meta.label.replace('Funding', '估算 Funding').replace('funding', '估算 funding') : meta.label,
              type: 'line' as const,
              data,
              showSymbol: false,
              sampling: 'lttb' as const,
              connectNulls: true,
              step: isTheoretical || key === 'funding_income_quote' ? ('end' as const) : undefined,
              lineStyle: {
                width:
                  key === 'nav_change_after_fee_quote' ||
                  key === 'funding_income_quote' ||
                  key === 'total_nav_after_fee_quote' ||
                  key.endsWith('after_fee_quote')
                    ? 2.4
                    : 1.6,
                color: meta.color,
                type: meta.lineType ?? ('solid' as const),
              },
              itemStyle: { color: meta.color },
              emphasis: { focus: 'series' as const },
            }
          })
        : (mode === 'symbols' ? symbolPoints : strategyPoints).map((item, index) => {
            const color = symbolPalette[index % symbolPalette.length]
            const key =
              feeMode === 'after'
                ? 'nav_change_after_fee_quote'
                : 'nav_change_before_fee_quote'
            const funding = mode !== 'symbols' ? strategyFunding?.find((row) => row.strategy === (item as StrategyNavTimeline).strategy) : undefined
            return {
              name:
                mode === 'symbols'
                  ? (item as SymbolNavTimeline).symbol
                  : strategyLabel((item as StrategyNavTimeline).strategy),
              type: 'line' as const,
              data: mode !== 'symbols' ? strategyFundingPoints(item as StrategyNavTimeline, funding, feeMode, fundingOnly) : item.points.map((point) => [
                point.ts_us / 1_000,
                point[key],
              ]),
              showSymbol: false,
              sampling: 'lttb' as const,
              connectNulls: true,
              step: fundingOnly ? ('end' as const) : undefined,
              markLine: fundingOnly && funding && index === 0 ? {
                silent: true,
                symbol: 'none',
                label: { show: false },
                lineStyle: { color: '#8993a4', type: 'dashed' as const },
                data: [{ yAxis: 0 }],
              } : undefined,
              lineStyle: { width: 1.7, color },
              itemStyle: { color },
              emphasis: { focus: 'series' as const },
            }
          })

    chart.setOption(
      {
        animation: false,
        textStyle: { fontFamily: UI_FONT_SANS },
        color:
          mode === 'portfolio'
            ? visibleSeries.map((key) => navSeriesMeta[key].color)
            : symbolPalette,
        grid: { left: fundingOnly ? 100 : 70, right: 24, top: 20, bottom: 74 },
        tooltip: {
          trigger: 'axis',
          confine: true,
          backgroundColor: 'rgba(255,255,255,0.97)',
          borderColor: '#d7dbe2',
          textStyle: { color: '#20252d', fontSize: 12 },
          valueFormatter: (value: unknown) =>
            (fundingOnly || mode === 'portfolio' && visibleSeries.includes('funding_income_quote') ? fundingMoney : money)(typeof value === 'number' ? value : Number(value)),
          axisPointer: {
            type: 'line',
            lineStyle: { color: '#8993a4', type: 'dashed' },
          },
        },
        xAxis: {
          type: 'time',
          boundaryGap: false,
          axisLine: { lineStyle: { color: '#d7dbe2' } },
          axisTick: { show: false },
          axisLabel: {
            color: '#697386',
            hideOverlap: true,
            formatter: (value: number) => chartTime(value),
          },
          splitLine: { show: false },
        },
        yAxis: {
          type: 'value',
          scale: !fundingOnly,
          axisLine: { show: false },
          axisTick: { show: false },
          axisLabel: {
            color: '#697386',
            formatter: (value: number) => fundingOnly ? fundingMoney(value) : money(value),
          },
          splitLine: { lineStyle: { color: '#edf0f4' } },
        },
        dataZoom: [
          { type: 'inside', filterMode: 'none' },
          {
            type: 'slider',
            height: 24,
            bottom: 20,
            borderColor: '#dfe3e8',
            backgroundColor: '#f5f6f8',
            fillerColor: 'rgba(31, 122, 104, 0.12)',
            handleStyle: { color: '#ffffff', borderColor: '#1f7a68' },
            moveHandleStyle: { color: '#8ab7ad' },
            dataBackground: {
              lineStyle: { color: '#9aa4b2' },
              areaStyle: { color: '#dfe3e8' },
            },
            selectedDataBackground: {
              lineStyle: { color: '#1f7a68' },
              areaStyle: { color: '#b9d9d1' },
            },
            textStyle: { color: '#697386', fontSize: 10 },
          },
        ],
        series,
      },
      true,
    )

    const observer = new ResizeObserver(() => chart.resize())
    observer.observe(containerRef.current)
    return () => {
      observer.disconnect()
      chart.dispose()
    }
  }, [
    feeMode,
    mode,
    points,
    strategyPoints,
    symbolPoints,
    theoretical,
    fundingPoints,
    fundingIsEstimate,
    strategyFunding,
    visibleSeries,
  ])

  return (
    <div
      ref={containerRef}
      className="pnl-chart"
      aria-label={
        mode === 'portfolio'
          ? '组合净值时间曲线'
          : mode === 'symbols'
            ? '分币净值时间曲线'
            : mode === 'strategyFunding'
              ? '分策略累计估算资金费时间曲线'
              : '分策略净值时间曲线'
      }
    />
  )
}
