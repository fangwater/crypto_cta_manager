import { BarChart, LineChart, ScatterChart } from 'echarts/charts'
import { DataZoomComponent, GridComponent, LegendComponent, MarkAreaComponent, MarkLineComponent, TooltipComponent } from 'echarts/components'
import * as echarts from 'echarts/core'
import { CanvasRenderer } from 'echarts/renderers'
import { useEffect, useRef, useState } from 'react'
import { money, quantity, timestampUs, UI_FONT_SANS } from '../format'
import { analyzePricePath, deltaKey, priceFormatter, rowPriceFormatter } from '../lib/acquisitionAnalysis'
import type { AcquisitionCostReport } from '../types'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from './ui/Card'
import { Input, Label, Select } from './ui/Field'

echarts.use([BarChart, LineChart, ScatterChart, DataZoomComponent, GridComponent, LegendComponent, MarkAreaComponent, MarkLineComponent, TooltipComponent, CanvasRenderer])

const worse = '#b84d55'
const better = '#2c8568'
const blue = '#2563a7'
const bps = (value: number | null) => value == null ? '--' : `${value >= 0 ? '+' : ''}${value.toFixed(2)} bps`
const seconds = (value: number | null) => value == null ? '--' : `${(value / 1e6).toFixed(2)} 秒`
const tooltip = { confine: true, renderMode: 'richText', backgroundColor: '#fff', borderColor: '#d7dbe2', textStyle: { color: '#20252d', fontSize: 12 } }
const grid = { left: 16, right: 24, top: 40, bottom: 24, containLabel: true }
const valueAxis = { type: 'value', splitLine: { lineStyle: { color: '#edf0f4' } }, axisLabel: { color: '#697386' } }

function CostChart({ option, label, height = 300, onDataClick }: { option: echarts.EChartsCoreOption; label: string; height?: number; onDataClick?: (index: number) => void }) {
  const ref = useRef<HTMLDivElement>(null)
  const chartRef = useRef<echarts.ECharts | null>(null)
  useEffect(() => {
    if (!ref.current) return
    const chart = echarts.init(ref.current, undefined, { renderer: 'canvas' })
    chartRef.current = chart
    const observer = new ResizeObserver(() => chart.resize())
    observer.observe(ref.current)
    return () => { observer.disconnect(); chart.dispose(); chartRef.current = null }
  }, [])
  useEffect(() => {
    chartRef.current?.setOption({ animation: false, textStyle: { fontFamily: UI_FONT_SANS }, ...option }, true)
  }, [option])
  useEffect(() => {
    const chart = chartRef.current
    if (!chart || !onDataClick) return
    const click = (event: unknown) => {
      const point = event as { componentType?: string; dataIndex?: number }
      if (point.componentType === 'series' && typeof point.dataIndex === 'number') onDataClick(point.dataIndex)
    }
    chart.on('click', click)
    return () => { chart.off('click', click) }
  }, [onDataClick])
  return <div ref={ref} style={{ height, width: '100%', minWidth: 0 }} role="img" aria-label={label} />
}

export function AcquisitionOverview({ report, onSelectSymbol }: { report: AcquisitionCostReport; onSelectSymbol: (symbol: string) => void }) {
  const [symbolMetric, setSymbolMetric] = useState('bps')
  const [delayMode, setDelayMode] = useState('target')
  const symbols = [...report.by_symbol].sort((a, b) => Math.abs(b.price_shortfall_usdt) - Math.abs(a.price_shortfall_usdt)).slice(0, 12).reverse()
  const delays = delayMode === 'target' ? report.by_target_delay : report.by_order_delay
  const turnover = delays.reduce((sum, row) => sum + row.reference_turnover_usdt, 0)
  const delayLabels: Record<string, string> = { '00_00-05s': '<5s', '01_05-10s': '5–10s', '02_10-30s': '10–30s', '03_30-60s': '30–60s', '04_60-300s': '60–300s', '05_after-300s': '≥300s' }
  return <div className="mb-6 grid min-w-0 gap-4 xl:grid-cols-2">
    <Card className="min-w-0">
      <CardHeader>
        <CardTitle>哪些币的滑点更明显</CardTitle>
        <CardDescription>全查询区间，按价格差金额绝对值选前 12 个币。bps 可跨币比较，正值更差、负值更好。点击柱形可筛选该币的 delta，优先查看价格差金额最大的目标。</CardDescription>
        <Label>显示指标<Select value={symbolMetric} onChange={(e) => setSymbolMetric(e.target.value)}><option value="bps">方向调整价格差 · bps</option><option value="amount">价格差金额 · U</option></Select></Label>
      </CardHeader>
      <CardContent>
        {symbols.length ? <CostChart label="各币种价格滑点对比，点击柱形筛选目标" onDataClick={(index) => { if (symbols[index]) onSelectSymbol(symbols[index].bucket) }} height={Math.max(280, symbols.length * 26 + 70)} option={{
          grid, tooltip: { ...tooltip, trigger: 'item', formatter: (p: { dataIndex: number }) => {
            const row = symbols[p.dataIndex]
            return `${row.bucket}\n价格差 ${bps(row.price_shortfall_bps)}\n价格差金额 ${money(row.price_shortfall_usdt)} U\n可比成交额 ${money(row.reference_turnover_usdt)} U\n${row.fill_count} 笔可比成交`
          } },
          xAxis: { ...valueAxis, name: symbolMetric === 'bps' ? 'bps' : 'U', nameLocation: 'middle', nameGap: 25 },
          yAxis: { type: 'category', data: symbols.map((row) => row.bucket), axisTick: { show: false }, axisLine: { show: false } },
          series: [{ type: 'bar', barMaxWidth: 18, data: symbols.map((row) => {
            const value = symbolMetric === 'bps' ? row.price_shortfall_bps : row.price_shortfall_usdt
            return { value, itemStyle: { color: value > 0 ? worse : better } }
          }) }],
        }} /> : <p className="py-12 text-center text-sm text-muted">没有可比成交。</p>}
      </CardContent>
    </Card>
    <Card className="min-w-0">
      <CardHeader>
        <CardTitle>成交延迟与滑点</CardTitle>
        <CardDescription>全区间可比成交。柱形为同量虚拟成交额占比，折线为该组价格差；相关性不代表延迟造成了全部滑点。</CardDescription>
        <Label>计时起点<Select value={delayMode} onChange={(e) => setDelayMode(e.target.value)}><option value="target">目标发布 → 成交</option><option value="order">订单信号 → 成交</option></Select></Label>
      </CardHeader>
      <CardContent>
        {delays.length ? <CostChart label="成交延迟分组占比及滑点" option={{
          grid: { ...grid, right: 36 }, legend: { top: 0, data: ['成交额占比', '价格差'] },
          tooltip: { ...tooltip, trigger: 'axis', formatter: (items: { dataIndex: number }[]) => {
            const row = delays[items[0].dataIndex]
            return `${delayLabels[row.bucket] ?? row.bucket}\n${row.fill_count} 笔 · ${money(row.reference_turnover_usdt)} U\n占比 ${(turnover ? row.reference_turnover_usdt / turnover * 100 : 0).toFixed(2)}%\n价格差 ${bps(row.price_shortfall_bps)} · ${money(row.price_shortfall_usdt)} U`
          } },
          xAxis: { type: 'category', data: delays.map((row) => delayLabels[row.bucket] ?? row.bucket), axisLabel: { interval: 0, fontSize: 10 } },
          yAxis: [{ ...valueAxis, name: '%' }, { ...valueAxis, name: 'bps', splitLine: { show: false } }],
          series: [
            { type: 'bar', name: '成交额占比', barMaxWidth: 28, itemStyle: { color: blue }, data: delays.map((row) => turnover ? row.reference_turnover_usdt / turnover * 100 : 0) },
            { type: 'line', name: '价格差', yAxisIndex: 1, itemStyle: { color: worse }, data: delays.map((row) => row.price_shortfall_bps) },
          ],
        }} /> : <p className="py-12 text-center text-sm text-muted">没有可比成交。</p>}
        <p className="mt-3 text-xs leading-6 text-muted">延迟按成交回报时间计算，不是网络耗时。订单计时使用稳定的订单信号时间；历史记录缺少该时间时会使用成交时间，订单延迟可能低估。</p>
      </CardContent>
    </Card>
  </div>
}

function Diagnostic({ title, detected, children }: { title: string; detected: boolean; children: React.ReactNode }) {
  return <div className={`rounded-lg border p-3 ${detected ? 'border-amber-300 bg-amber-50' : 'border-border-soft bg-canvas/50'}`}>
    <div className="mb-2 text-sm font-medium">{title}</div>
    <div className="text-xs leading-6 text-muted">{children}</div>
  </div>
}

export function AcquisitionDeltaAnalysis({ report, selectedKey, onSelect }: { report: AcquisitionCostReport; selectedKey: string; onSelect: (key: string) => void }) {
  const [priceMode, setPriceMode] = useState('bps')
  const [moveThreshold, setMoveThreshold] = useState(10)
  const [slowThreshold, setSlowThreshold] = useState(60)
  const row = report.rows.find((candidate) => deltaKey(candidate) === selectedKey) ?? report.rows.at(-1)
  if (!row) return null
  const formatPrice = rowPriceFormatter(row)
  const path = analyzePricePath(row)
  const execution = row.execution
  const sign = Math.sign(row.delta_qty)
  const offset = (ts: number) => (ts - row.received_at_us) / 1e6
  const plotPrice = (price: number) => priceMode === 'price' ? price : sign * (price / row.virtual_vwap - 1) * 10_000
  const startUs = row.virtual_execution_ts_us - 300e6
  const xEnd = Math.max(offset(row.virtual_execution_ts_us), offset(execution.last_fill_ts_us ?? row.received_at_us))
  const reversal = path.reversal
  const reversed = reversal != null && Math.min(Math.abs(reversal.outwardBps), Math.abs(reversal.returnBps)) >= moveThreshold
  const jumped = path.jumpBps != null && Math.abs(path.jumpBps) >= moveThreshold
  const slow = execution.qty_weighted_target_delay_us != null && execution.qty_weighted_target_delay_us >= slowThreshold * 1e6
  const late = execution.after_window_qty_ratio != null && execution.after_window_qty_ratio > 0
  const maxQty = Math.max(...execution.fills.map((fill) => Math.abs(fill.actual_qty)), 1e-30)
  const priceDifference = row.actual_vwap == null ? null : row.actual_vwap - row.virtual_vwap
  const differenceFormat = priceFormatter([priceDifference])
  const series: echarts.EChartsCoreOption['series'] = [
    {
      type: 'line', name: '分钟均价', connectNulls: false, symbolSize: 8, itemStyle: { color: blue },
      data: row.sample_prices.map((price, i) => [offset(startUs + (i * 60 + 30) * 1e6), price == null ? null : plotPrice(price)]),
      tooltip: { formatter: (p: { dataIndex: number }) => {
        const i = p.dataIndex
        return `第 ${i + 1} 个完整分钟\n发布后 ${offset(startUs + i * 60e6).toFixed(1)}–${offset(startUs + (i + 1) * 60e6).toFixed(1)} 秒\n${row.zero_volume_fallback_sample_count ? '收盘价' : '分钟 VWAP'} ${formatPrice(row.sample_prices[i])}`
      } },
      markArea: { silent: true, itemStyle: { color: 'rgba(37,99,167,0.05)' }, data: [[{ xAxis: offset(startUs) }, { xAxis: offset(row.virtual_execution_ts_us) }]] },
      markLine: { silent: true, symbol: 'none', label: { show: false }, lineStyle: { color: '#9ca3af', type: 'dotted' }, data: [{ xAxis: offset(row.virtual_execution_ts_us) }] },
    },
    { type: 'line', name: '理论 TWAP', symbol: 'none', lineStyle: { color: better, type: 'dashed' }, data: [[0, plotPrice(row.virtual_vwap)], [xEnd, plotPrice(row.virtual_vwap)]] },
    ...(row.actual_vwap == null ? [] : [{ type: 'line', name: '实际 VWAP', symbol: 'none', lineStyle: { color: '#b7791f', type: 'dashed' }, data: [[0, plotPrice(row.actual_vwap)], [xEnd, plotPrice(row.actual_vwap)]] }]),
    ...(['maker', 'taker', 'unclassified', 'internal_cross'] as const).map((role, index) => ({
      type: 'scatter', name: ['Maker 成交', 'Taker 成交', '未分类成交', '内部对敲'][index], itemStyle: { color: [better, worse, '#7357a3', '#64748b'][index], opacity: 0.85 },
      data: execution.fills.filter((fill) => fill.liquidity === role).map((fill) => ({
        value: [offset(fill.fill_ts_us), plotPrice(fill.actual_price)],
        symbolSize: 7 + Math.sqrt(Math.abs(fill.actual_qty) / maxQty) * 13,
        detail: `发布后 ${seconds(fill.target_delay_us)}\n订单后 ${seconds(fill.order_delay_us)}\n成交价 ${formatPrice(fill.actual_price)}\n数量 ${quantity(Math.abs(fill.actual_qty))}\n方向调整价差 ${bps(sign * (fill.actual_price / row.virtual_vwap - 1) * 10_000)}`,
      })),
      tooltip: { formatter: (p: { data: { detail: string } }) => p.data.detail },
    })),
  ]
  return <Card className="mb-6 min-w-0 scroll-mt-4" id="delta-analysis">
    <CardHeader>
      <CardTitle>单个目标复盘</CardTitle>
      <CardDescription>同一个币、同一个目标下对齐分钟价格与实际成交。下面的线索可以同时成立；只描述观测到的形态，不做确定的因果归因。</CardDescription>
    </CardHeader>
    <CardContent className="min-w-0 space-y-4">
      <div className="grid min-w-0 gap-3 sm:grid-cols-2">
        <Label className="min-w-0">当前页目标<Select value={deltaKey(row)} onChange={(e) => onSelect(e.target.value)} className="w-full min-w-0">
          {[...report.rows].reverse().map((item) => <option key={deltaKey(item)} value={deltaKey(item)}>{item.symbol} · {item.delta_qty > 0 ? '买' : '卖'} · {item.strategy_name} · {timestampUs(item.received_at_us)} · 价差 {item.price_shortfall_usdt == null ? '--' : `${money(item.price_shortfall_usdt)} U`}</option>)}
        </Select></Label>
        <Label>价格坐标<Select value={priceMode} onChange={(e) => setPriceMode(e.target.value)}><option value="bps">相对理论价 · 方向调整 bps</option><option value="price">币种原始价格</option></Select></Label>
      </div>
      <div className="grid gap-3 text-sm sm:grid-cols-3">
        <div><div className="text-xs text-muted">理论 TWAP / 实际 VWAP</div><div className="mt-1 break-all font-medium tabular-nums">{formatPrice(row.virtual_vwap)} / {formatPrice(row.actual_vwap)}</div></div>
        <div><div className="text-xs text-muted">实际 − 理论（原始价格差）</div><div className="mt-1 break-all font-medium tabular-nums">{priceDifference != null && priceDifference > 0 ? '+' : ''}{differenceFormat(priceDifference)}</div></div>
        <div><div className="text-xs text-muted">方向调整价格差（正值更差）</div><div className={`mt-1 font-medium tabular-nums ${(row.price_shortfall_bps ?? 0) > 0 ? 'number-negative' : 'number-positive'}`}>{bps(row.price_shortfall_bps)}</div></div>
      </div>
      <CostChart height={360} label={`${row.symbol} 五分钟均价与实际成交时点`} option={{
        grid: { ...grid, top: 64, bottom: 80, right: 20 }, tooltip: { ...tooltip, trigger: 'item', formatter: (p: { seriesName: string; value: [number, number] }) => `${p.seriesName}\n${priceMode === 'price' ? formatPrice(p.value[1]) : bps(p.value[1])}` },
        legend: { type: 'scroll', top: 0, textStyle: { fontSize: 11 } },
        xAxis: { ...valueAxis, min: 0, max: Math.ceil(xEnd), name: '目标发布后 / 秒', nameLocation: 'middle', nameGap: 30 },
        yAxis: { ...valueAxis, scale: true, name: priceMode === 'price' ? '价格' : '方向调整 bps', axisLabel: { formatter: (value: number) => priceMode === 'price' ? formatPrice(value) : value.toFixed(1), fontSize: 10 } },
        dataZoom: [{ type: 'inside', filterMode: 'none' }, { type: 'slider', bottom: 0, height: 20, filterMode: 'none' }], series,
      }} />
      <p className="text-xs leading-6 text-muted">阴影为目标后的五个完整分钟，右边界为窗口结束。均价画在分钟中点，连线只帮助观察走势，不代表逐笔路径；空白分钟不连线。成交点大小表示数量。{execution.fills_sampled ? `图中抽样 ${execution.fills.length} / ${row.matched_fill_count} 笔，保留首尾及价格极值；下方统计使用全部配对成交。` : `共 ${row.matched_fill_count} 笔配对成交。`}成交统计仅覆盖本次查询范围，不代表目标的全部生命周期。价格仅调整显示精度，不代表交易所 tick 精度。</p>
      <div className="grid gap-3 text-sm sm:grid-cols-3">
        <div><div className="text-xs text-muted">目标 → 成交 · 数量加权平均</div><div className="mt-1 tabular-nums">{seconds(execution.qty_weighted_target_delay_us)}</div></div>
        <div><div className="text-xs text-muted">订单 → 成交 · 数量加权平均</div><div className="mt-1 tabular-nums">{seconds(execution.qty_weighted_order_delay_us)}</div></div>
        <div><div className="text-xs text-muted">窗口结束后成交量占比</div><div className="mt-1 tabular-nums">{execution.after_window_qty_ratio == null ? '--' : `${(execution.after_window_qty_ratio * 100).toFixed(1)}%`} · 完成率 {(row.fill_ratio * 100).toFixed(1)}%</div></div>
      </div>
      <div className="grid gap-3 sm:grid-cols-2">
        <Label>价格形态阈值 · bps<Input type="number" min="0.1" step="1" value={moveThreshold} onChange={(e) => { const v = Number(e.target.value); if (Number.isFinite(v) && v >= 0.1) setMoveThreshold(v) }} /></Label>
        <Label>平均成交延迟阈值 · 秒<Input type="number" min="1" step="10" value={slowThreshold} onChange={(e) => { const v = Number(e.target.value); if (Number.isFinite(v) && v >= 1) setSlowThreshold(v) }} /></Label>
      </div>
      <div className="grid gap-3 lg:grid-cols-3">
        <Diagnostic title={!path.complete ? '反转：数据不足' : reversed ? '五分钟内存在反转线索' : '未见显著反转'} detected={reversed}>
          {!path.complete ? '需要五个都有成交的分钟。跳过分钟或全空窗口的收盘价不能用于反转判断。' : reversal ? `第 ${reversal.minute} 分钟附近转向：先${reversal.outwardBps > 0 ? '涨' : '跌'} ${Math.abs(reversal.outwardBps).toFixed(2)} bps，再${reversal.returnBps > 0 ? '涨' : '跌'} ${Math.abs(reversal.returnBps).toFixed(2)} bps。两段都达到 ${moveThreshold} bps 才标记。` : `首分钟到内部转折点、再到末分钟，未发现两段方向相反且都达到 ${moveThreshold} bps 的形态。`}
        </Diagnostic>
        <Diagnostic title={path.jumpBps == null ? '跳变：数据不足' : jumped ? '存在分钟级跳变线索' : '未见显著分钟跳变'} detected={jumped}>
          {path.jumpBps == null ? '没有相邻且都有成交的分钟可供比较。' : `第 ${path.jumpMinute! - 1} → ${path.jumpMinute} 分钟均价变化最大，为 ${bps(path.jumpBps)}，判断阈值 ${moveThreshold} bps。分钟均价无法确认瞬时跳价，需要更细行情才能区分突跳与连续移动。`}
        </Diagnostic>
        <Diagnostic title={execution.qty_weighted_target_delay_us == null ? '速度：暂无配对成交' : slow || late ? '存在成交延迟线索' : '平均成交延迟在阈值内'} detected={slow || late}>
          {execution.qty_weighted_target_delay_us == null ? '未配对不代表未成交；检查参考覆盖率和缺失行情。' : `数量加权平均 ${seconds(execution.qty_weighted_target_delay_us)}，阈值 ${slowThreshold} 秒；窗口结束后成交占 ${(100 * (execution.after_window_qty_ratio ?? 0)).toFixed(1)}%。这只是速度筛查，需结合订单算法的预期节奏和同图价格判断是否造成不利成交。`}
        </Diagnostic>
      </div>
      <div className="rounded-lg bg-canvas p-3 text-xs leading-6 text-muted">
        价格差拆解（方向调整）：实际相对首个计价分钟 <strong>{bps(path.versusFirstBps)}</strong> ＋ 首分钟相对五分钟理论均价 <strong>{bps(path.pathBps)}</strong> ＝ 总价格差 <strong>{bps(row.price_shortfall_bps)}</strong>。后半项反映基准随市场路径变化的影响；前半项也包含市场变化，不等于纯执行损失。两项均不含手续费。
      </div>
    </CardContent>
  </Card>
}
