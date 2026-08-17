import { LineChart } from 'echarts/charts'
import {
  DataZoomComponent,
  GridComponent,
  TooltipComponent,
} from 'echarts/components'
import * as echarts from 'echarts/core'
import { CanvasRenderer } from 'echarts/renderers'
import { useEffect, useRef } from 'react'
import type {
  FeeMode,
  NavSeriesKey,
  NavTimelinePoint,
  StrategyNavTimeline,
  SymbolNavTimeline,
  TimelineChartMode,
} from '../types'
import { strategyLabel } from '../format'

echarts.use([
  LineChart,
  DataZoomComponent,
  GridComponent,
  TooltipComponent,
  CanvasRenderer,
])

interface Props {
  points: NavTimelinePoint[]
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
  { label: string; color: string; dashed?: boolean }
> = {
  nav_change_before_fee_quote: { label: '费前净值', color: '#2563a7' },
  nav_change_after_fee_quote: { label: '费后净值', color: '#176b5b' },
  realized_pnl_before_fee_quote: { label: '已实现盈亏', color: '#b7791f' },
  floating_pnl_quote: {
    label: '浮动盈亏',
    color: '#4b6478',
    dashed: true,
  },
  estimated_trading_fee_quote: {
    label: '估算手续费',
    color: '#c2413b',
    dashed: true,
  },
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
    const series =
      mode === 'portfolio'
        ? visibleSeries.map((key) => {
            const meta = navSeriesMeta[key]
            return {
              name: meta.label,
              type: 'line' as const,
              data: points.map((point) => [point.ts_us / 1_000, point[key]]),
              showSymbol: false,
              sampling: 'lttb' as const,
              connectNulls: true,
              lineStyle: {
                width: key === 'nav_change_after_fee_quote' ? 2.4 : 1.6,
                color: meta.color,
                type: meta.dashed ? ('dashed' as const) : ('solid' as const),
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
            return {
              name:
                mode === 'symbols'
                  ? (item as SymbolNavTimeline).symbol
                  : strategyLabel((item as StrategyNavTimeline).strategy),
              type: 'line' as const,
              data: item.points.map((point) => [
                point.ts_us / 1_000,
                point[key],
              ]),
              showSymbol: false,
              sampling: 'lttb' as const,
              connectNulls: true,
              lineStyle: { width: 1.7, color },
              itemStyle: { color },
              emphasis: { focus: 'series' as const },
            }
          })

    chart.setOption(
      {
        animation: false,
        color:
          mode === 'portfolio'
            ? visibleSeries.map((key) => navSeriesMeta[key].color)
            : symbolPalette,
        grid: { left: 70, right: 24, top: 20, bottom: 74 },
        tooltip: {
          trigger: 'axis',
          confine: true,
          backgroundColor: 'rgba(255,255,255,0.97)',
          borderColor: '#d7dbe2',
          textStyle: { color: '#20252d', fontSize: 12 },
          valueFormatter: (value: unknown) =>
            money(typeof value === 'number' ? value : Number(value)),
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
          scale: true,
          axisLine: { show: false },
          axisTick: { show: false },
          axisLabel: {
            color: '#697386',
            formatter: (value: number) => money(value),
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
  }, [feeMode, mode, points, strategyPoints, symbolPoints, visibleSeries])

  return (
    <div
      ref={containerRef}
      className="pnl-chart"
      aria-label={
        mode === 'portfolio'
          ? '组合净值时间曲线'
          : mode === 'symbols'
            ? '分币净值时间曲线'
            : '分策略净值时间曲线'
      }
    />
  )
}
