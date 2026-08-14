import { BarChart } from 'echarts/charts'
import {
  GridComponent,
  LegendComponent,
  TooltipComponent,
} from 'echarts/components'
import * as echarts from 'echarts/core'
import { CanvasRenderer } from 'echarts/renderers'
import { useEffect, useRef } from 'react'
import { compactMoney, money } from '../format'
import type { ChartMode, FeeMode, SymbolRow } from '../types'

echarts.use([
  BarChart,
  GridComponent,
  LegendComponent,
  TooltipComponent,
  CanvasRenderer,
])

interface Props {
  rows: SymbolRow[]
  chartMode: ChartMode
  feeMode: FeeMode
}

export function SymbolChart({ rows, chartMode, feeMode }: Props) {
  const elementRef = useRef<HTMLDivElement>(null)
  const chartRef = useRef<echarts.ECharts | null>(null)

  useEffect(() => {
    const element = elementRef.current
    if (!element) return
    const chart = echarts.init(element, undefined, { renderer: 'canvas' })
    chartRef.current = chart
    const resizeObserver = new ResizeObserver(() => chart.resize())
    resizeObserver.observe(element)
    return () => {
      resizeObserver.disconnect()
      chart.dispose()
      chartRef.current = null
    }
  }, [])

  useEffect(() => {
    const chart = chartRef.current
    if (!chart) return
    const ordered = [...rows]
      .sort((left, right) => {
        const leftValue =
          chartMode === 'nav'
            ? feeMode === 'after'
              ? left.nav_change_after_fee_quote
              : left.nav_change_before_fee_quote
            : left.net_position_value_quote
        const rightValue =
          chartMode === 'nav'
            ? feeMode === 'after'
              ? right.nav_change_after_fee_quote
              : right.nav_change_before_fee_quote
            : right.net_position_value_quote
        return Math.abs(leftValue) - Math.abs(rightValue)
      })
      .slice(-14)
    const values = ordered.map((row) => {
      if (chartMode === 'exposure') return row.net_position_value_quote
      return feeMode === 'after'
        ? row.nav_change_after_fee_quote
        : row.nav_change_before_fee_quote
    })

    chart.setOption(
      {
        animationDuration: 260,
        grid: { left: 104, right: 26, top: 22, bottom: 42 },
        tooltip: {
          trigger: 'axis',
          axisPointer: { type: 'shadow' },
          borderWidth: 1,
          borderColor: '#d9dde3',
          backgroundColor: '#ffffff',
          textStyle: { color: '#20252d', fontSize: 12 },
          formatter: (items: unknown) => {
            const item = Array.isArray(items) ? items[0] : null
            if (!item || typeof item !== 'object') return ''
            const record = item as { axisValue?: string; value?: number }
            return `${record.axisValue ?? ''}<br/><strong>${money(Number(record.value ?? 0))} USDT</strong>`
          },
        },
        xAxis: {
          type: 'value',
          axisLine: { lineStyle: { color: '#cfd4dc' } },
          axisTick: { show: false },
          splitLine: { lineStyle: { color: '#edf0f3' } },
          axisLabel: {
            color: '#737b87',
            fontSize: 11,
            formatter: (value: number) => compactMoney(value),
          },
        },
        yAxis: {
          type: 'category',
          data: ordered.map((row) => row.symbol.replace(/USDT$/, '')),
          axisLine: { show: false },
          axisTick: { show: false },
          axisLabel: { color: '#3d444e', fontSize: 11, margin: 12 },
        },
        series: [
          {
            type: 'bar',
            data: values.map((value) => ({
              value,
              itemStyle: { color: value >= 0 ? '#2c8568' : '#b84d55' },
            })),
            barMaxWidth: 18,
            itemStyle: { borderRadius: 2 },
          },
        ],
      },
      true,
    )
  }, [chartMode, feeMode, rows])

  return <div ref={elementRef} className="symbol-chart" role="img" />
}
