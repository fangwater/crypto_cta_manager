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
import type { PositionHistoryPortfolioPoint, PositionHistorySymbolPoint } from '../types'
import { UI_FONT_SANS } from '../format'

echarts.use([
  LineChart,
  DataZoomComponent,
  GridComponent,
  LegendComponent,
  TooltipComponent,
  CanvasRenderer,
])

type SymbolSeries = {
  name: string
  points: PositionHistorySymbolPoint[]
}

type Props =
  | {
      kind: 'notional'
      points: PositionHistoryPortfolioPoint[]
    }
  | {
      kind: 'leverage'
      points: PositionHistoryPortfolioPoint[]
    }
  | {
      kind: 'quantity'
      series: SymbolSeries[]
    }
  | {
      kind: 'symbol-notional'
      series: SymbolSeries[]
    }
  | {
      kind: 'symbol-leverage'
      series: SymbolSeries[]
    }

const palette = [
  '#176b5b',
  '#2563a7',
  '#b7791f',
  '#c2413b',
  '#7357a3',
  '#0f766e',
  '#9c4f87',
  '#4b6478',
  '#d97706',
  '#4467a8',
]

function compactNumber(value: number, digits = 2) {
  return value.toLocaleString('en-US', {
    maximumFractionDigits: digits,
    minimumFractionDigits: 0,
  })
}

function timeLabel(value: number) {
  const date = new Date(value)
  const month = String(date.getMonth() + 1).padStart(2, '0')
  const day = String(date.getDate()).padStart(2, '0')
  const hour = String(date.getHours()).padStart(2, '0')
  const minute = String(date.getMinutes()).padStart(2, '0')
  return `${month}-${day}\n${hour}:${minute}`
}

export function PositionHistoryChart(props: Props) {
  const containerRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (!containerRef.current) return
    const chart = echarts.init(containerRef.current, undefined, { renderer: 'canvas' })
    const shared = {
      type: 'line' as const,
      showSymbol: false,
      sampling: 'lttb' as const,
      connectNulls: false,
      emphasis: { focus: 'series' as const },
    }

    const option =
      props.kind === 'notional'
        ? {
            color: ['#2563a7'],
            legend: { show: false },
            yAxis: [valueAxis('USDT')],
            series: [
              {
                ...shared,
                name: '总名义仓位',
                data: props.points.map((point) => [
                  point.ts_ms,
                  point.gross_notional_usdt,
                ]),
                lineStyle: { width: 2.2, color: '#2563a7' },
                itemStyle: { color: '#2563a7' },
                areaStyle: { color: 'rgba(37, 99, 167, 0.10)' },
              },
            ],
          }
        : props.kind === 'leverage'
          ? {
              color: ['#b7791f'],
              legend: { show: false },
              yAxis: [valueAxis('x')],
              series: [
                {
                  ...shared,
                  name: '按当前权益计算的杠杆率',
                  data: props.points.map((point) => [
                    point.ts_ms,
                    point.gross_leverage,
                  ]),
                  lineStyle: { width: 1.8, color: '#b7791f' },
                  itemStyle: { color: '#b7791f' },
                },
              ],
            }
          : {
              color: palette,
              legend: {
                type: 'scroll' as const,
                top: 2,
                left: 10,
                right: 10,
                textStyle: { color: '#697386', fontSize: 11 },
              },
              yAxis: [
                valueAxis(
                  props.kind === 'quantity'
                    ? 'Qty'
                    : props.kind === 'symbol-leverage'
                      ? 'x'
                      : 'USDT',
                ),
              ],
              series: props.series.map((item, index) => ({
                ...shared,
                name: item.name,
                data: item.points.map((point) => [
                  point.ts_ms,
                  props.kind === 'quantity'
                    ? point.quantity
                    : props.kind === 'symbol-leverage'
                      ? point.leverage_contribution
                      : point.gross_notional_usdt,
                ]),
                lineStyle: { width: 1.7, color: palette[index % palette.length] },
                itemStyle: { color: palette[index % palette.length] },
              })),
            }

    chart.setOption(
      {
        animation: false,
        textStyle: { fontFamily: UI_FONT_SANS },
        grid: {
          left: 74,
          right: 24,
          top:
            props.kind === 'quantity' ||
            props.kind === 'symbol-notional' ||
            props.kind === 'symbol-leverage'
              ? 44
              : 22,
          bottom: 72,
        },
        tooltip: {
          trigger: 'axis',
          confine: true,
          backgroundColor: 'rgba(255,255,255,0.97)',
          borderColor: '#d7dbe2',
          textStyle: { color: '#20252d', fontSize: 12 },
          valueFormatter: (value: unknown) => compactNumber(Number(value), 4),
          axisPointer: { type: 'line', lineStyle: { color: '#8993a4', type: 'dashed' } },
        },
        xAxis: {
          type: 'time',
          boundaryGap: false,
          axisLine: { lineStyle: { color: '#d7dbe2' } },
          axisTick: { show: false },
          axisLabel: { color: '#697386', hideOverlap: true, formatter: timeLabel },
          splitLine: { show: false },
        },
        dataZoom: [
          { type: 'inside', filterMode: 'none' },
          {
            type: 'slider',
            height: 24,
            bottom: 18,
            borderColor: '#dfe3e8',
            backgroundColor: '#f5f6f8',
            fillerColor: 'rgba(31, 122, 104, 0.12)',
            handleStyle: { color: '#ffffff', borderColor: '#1f7a68' },
            moveHandleStyle: { color: '#8ab7ad' },
            textStyle: { color: '#697386', fontSize: 10 },
          },
        ],
        ...option,
      },
      true,
    )

    const observer = new ResizeObserver(() => chart.resize())
    observer.observe(containerRef.current)
    return () => {
      observer.disconnect()
      chart.dispose()
    }
  }, [props])

  return <div ref={containerRef} className="h-[360px] w-full" />
}

function valueAxis(name: string, position?: 'right') {
  return {
    type: 'value' as const,
    scale: true,
    name,
    position,
    nameTextStyle: { color: '#697386', fontSize: 10 },
    axisLine: { show: false },
    axisTick: { show: false },
    axisLabel: { color: '#697386', formatter: (value: number) => compactNumber(value) },
    splitLine: { lineStyle: { color: '#edf0f4' } },
  }
}
