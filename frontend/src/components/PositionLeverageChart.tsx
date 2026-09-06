import { LineChart } from 'echarts/charts'
import { DataZoomComponent, GridComponent, TooltipComponent } from 'echarts/components'
import * as echarts from 'echarts/core'
import { CanvasRenderer } from 'echarts/renderers'
import { useEffect, useRef } from 'react'
import type { NavTimelinePoint, SymbolNavTimeline } from '../types'
import { UI_FONT_SANS } from '../format'

echarts.use([LineChart, DataZoomComponent, GridComponent, TooltipComponent, CanvasRenderer])

interface Props {
  points: NavTimelinePoint[]
  symbolPoints: SymbolNavTimeline[]
  mode: 'portfolio' | 'symbols'
  equityUsdt: number | null
}

const palette = ['#176b5b', '#2563a7', '#b7791f', '#c2413b', '#7357a3', '#0f766e']

function money(value: number) {
  return value.toLocaleString('en-US', { maximumFractionDigits: 0 })
}

export function PositionLeverageChart({ points, symbolPoints, mode, equityUsdt }: Props) {
  const containerRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (!containerRef.current) return
    const chart = echarts.init(containerRef.current, undefined, { renderer: 'canvas' })
    const leverage = (point: NavTimelinePoint) =>
      equityUsdt && equityUsdt > 0 ? point.gross_position_value_quote / equityUsdt : null
    const positionSeries =
      mode === 'portfolio'
        ? [
            {
              name: '总仓位',
              type: 'line' as const,
              data: points.map((point) => [point.ts_us / 1_000, point.gross_position_value_quote]),
              lineStyle: { width: 2, color: '#176b5b' },
              itemStyle: { color: '#176b5b' },
            },
            {
              name: '净仓位',
              type: 'line' as const,
              data: points.map((point) => [point.ts_us / 1_000, point.net_position_value_quote]),
              lineStyle: { width: 1.5, color: '#2563a7', type: 'dashed' as const },
              itemStyle: { color: '#2563a7' },
            },
          ]
        : symbolPoints.map((series, index) => {
            const color = palette[index % palette.length]
            return {
              name: series.symbol,
              type: 'line' as const,
              data: series.points.map((point) => [point.ts_us / 1_000, point.net_position_value_quote]),
              lineStyle: { width: 1.6, color },
              itemStyle: { color },
            }
          })
    const series = [
      ...positionSeries.map((item) => ({ ...item, showSymbol: false, sampling: 'lttb' as const, yAxisIndex: 0 })),
      {
        name: '杠杆率',
        type: 'line' as const,
        data: points.map((point) => [point.ts_us / 1_000, leverage(point)]),
        showSymbol: false,
        sampling: 'lttb' as const,
        connectNulls: false,
        yAxisIndex: 1,
        lineStyle: { width: 1.7, color: '#b7791f' },
        itemStyle: { color: '#b7791f' },
      },
    ]

    chart.setOption({
      animation: false,
      textStyle: { fontFamily: UI_FONT_SANS },
      grid: { left: 70, right: 70, top: 28, bottom: 74 },
      tooltip: {
        trigger: 'axis',
        confine: true,
        backgroundColor: 'rgba(255,255,255,0.97)',
        borderColor: '#d7dbe2',
        textStyle: { color: '#20252d', fontSize: 12 },
        formatter: (params: unknown) => {
          const rows = Array.isArray(params) ? params : [params]
          return rows
            .map((row: { marker?: string; seriesName?: string; value?: unknown[] }) => {
              const value = Number(row.value?.[1])
              const formatted = row.seriesName === '杠杆率'
                ? `${value.toFixed(3)}x`
                : `${money(value)} USDT`
              return `${row.marker ?? ''}${row.seriesName ?? ''}: ${formatted}`
            })
            .join('<br/>')
        },
      },
      xAxis: { type: 'time', boundaryGap: false, axisLine: { lineStyle: { color: '#d7dbe2' } }, axisTick: { show: false }, axisLabel: { color: '#697386', hideOverlap: true }, splitLine: { show: false } },
      yAxis: [
        { type: 'value', scale: true, axisLine: { show: false }, axisTick: { show: false }, axisLabel: { color: '#697386', formatter: money }, splitLine: { lineStyle: { color: '#edf0f4' } } },
        { type: 'value', scale: true, axisLine: { show: false }, axisTick: { show: false }, axisLabel: { color: '#b7791f', formatter: (value: number) => `${value.toFixed(2)}x` }, splitLine: { show: false } },
      ],
      dataZoom: [
        { type: 'inside', filterMode: 'none' },
        { type: 'slider', height: 24, bottom: 20, borderColor: '#dfe3e8', backgroundColor: '#f5f6f8', fillerColor: 'rgba(31, 122, 104, 0.12)', handleStyle: { color: '#ffffff', borderColor: '#1f7a68' }, textStyle: { color: '#697386', fontSize: 10 } },
      ],
      series,
    }, true)
    const observer = new ResizeObserver(() => chart.resize())
    observer.observe(containerRef.current)
    return () => {
      observer.disconnect()
      chart.dispose()
    }
  }, [equityUsdt, mode, points, symbolPoints])

  return <div ref={containerRef} className="pnl-chart" aria-label="仓位与杠杆率时间曲线" />
}
