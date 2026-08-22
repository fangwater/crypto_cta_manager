import { LineChart } from 'echarts/charts'
import {
  DataZoomComponent,
  GridComponent,
  LegendComponent,
  TooltipComponent,
} from 'echarts/components'
import * as echarts from 'echarts/core'
import { CanvasRenderer } from 'echarts/renderers'
import { useEffect, useRef } from 'react'
import { money, UI_FONT_SANS } from '../format'
import type { ExecutionCostPoint } from '../types'

echarts.use([
  LineChart,
  DataZoomComponent,
  GridComponent,
  LegendComponent,
  TooltipComponent,
  CanvasRenderer,
])

interface Props {
  points: ExecutionCostPoint[]
}

const series = [
  {
    key: 'twap_price_slippage_on_filled_usdt' as const,
    label: 'TWAP 价格滑点',
    color: '#2563a7',
    dashed: true,
  },
  {
    key: 'actual_price_slippage_usdt' as const,
    label: '实际价格滑点',
    color: '#b7791f',
  },
  {
    key: 'shortfall_vs_twap_usdt' as const,
    label: '实际相对 TWAP',
    color: '#176b5b',
  },
]

function chartTime(value: number) {
  const date = new Date(value)
  const month = String(date.getMonth() + 1).padStart(2, '0')
  const day = String(date.getDate()).padStart(2, '0')
  const hour = String(date.getHours()).padStart(2, '0')
  const minute = String(date.getMinutes()).padStart(2, '0')
  return `${month}-${day}\n${hour}:${minute}`
}

export function ExecutionCostChart({ points }: Props) {
  const elementRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    const element = elementRef.current
    if (!element) return
    const chart = echarts.init(element, undefined, { renderer: 'canvas' })
    chart.setOption({
      animation: false,
      textStyle: { fontFamily: UI_FONT_SANS },
      color: series.map((item) => item.color),
      grid: { left: 72, right: 24, top: 54, bottom: 70 },
      legend: {
        top: 8,
        textStyle: { color: '#4b5563', fontSize: 11 },
      },
      tooltip: {
        trigger: 'axis',
        confine: true,
        backgroundColor: 'rgba(255,255,255,0.97)',
        borderColor: '#d7dbe2',
        textStyle: { color: '#20252d', fontSize: 12 },
        valueFormatter: (value: unknown) => `${money(Number(value))} USDT`,
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
        axisLabel: { color: '#697386', formatter: (value: number) => money(value) },
        splitLine: { lineStyle: { color: '#edf0f4' } },
      },
      dataZoom: [
        { type: 'inside', filterMode: 'none' },
        {
          type: 'slider',
          height: 22,
          bottom: 18,
          borderColor: '#dfe3e8',
          backgroundColor: '#f5f6f8',
          fillerColor: 'rgba(31, 122, 104, 0.12)',
          handleStyle: { color: '#ffffff', borderColor: '#1f7a68' },
          textStyle: { color: '#697386', fontSize: 10 },
        },
      ],
      series: series.map((item) => ({
        name: item.label,
        type: 'line',
        data: points.map((point) => [point.ts_us / 1_000, point[item.key]]),
        showSymbol: false,
        sampling: 'lttb',
        lineStyle: {
          width: item.key === 'shortfall_vs_twap_usdt' ? 2.4 : 1.6,
          color: item.color,
          type: item.dashed ? 'dashed' : 'solid',
        },
        itemStyle: { color: item.color },
        emphasis: { focus: 'series' },
      })),
    })
    const observer = new ResizeObserver(() => chart.resize())
    observer.observe(element)
    return () => {
      observer.disconnect()
      chart.dispose()
    }
  }, [points])

  return (
    <div
      ref={elementRef}
      className="h-[360px] w-full sm:h-[420px]"
      role="img"
      aria-label="累计价格执行滑点时间序列"
    />
  )
}
