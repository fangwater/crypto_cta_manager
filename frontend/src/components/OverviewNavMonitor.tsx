import { useEffect, useRef, useState } from 'react'
import * as echarts from 'echarts/core'
import { LineChart } from 'echarts/charts'
import { DataZoomComponent, GridComponent, LegendComponent, MarkLineComponent, TooltipComponent } from 'echarts/components'
import { CanvasRenderer } from 'echarts/renderers'
import { RefreshCw } from 'lucide-react'
import { ApiError, getNavMonitor, type NavMonitor } from '../api'
import { money } from '../format'
import type { OverviewRow } from '../lib/accountOverview'
import { Button } from './ui/Button'

echarts.use([LineChart, DataZoomComponent, GridComponent, LegendComponent, MarkLineComponent, TooltipComponent, CanvasRenderer])
const time = new Intl.DateTimeFormat('zh-CN', { timeZone: 'Asia/Shanghai', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', hourCycle: 'h23' })
const fullTime = new Intl.DateTimeFormat('zh-CN', { timeZone: 'Asia/Shanghai', year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', hourCycle: 'h23' })
type Entry = { value?: NavMonitor; error?: string; stale?: boolean }

export function OverviewNavMonitor({ accounts, autoRefresh }: { accounts: OverviewRow[]; autoRefresh: boolean }) {
  const [entries, setEntries] = useState<Record<string, Entry>>({})
  const [revision, setRevision] = useState(0)
  const [refreshing, setRefreshing] = useState(false)
  const scope = JSON.stringify(accounts.map(a => a.source_id).sort())
  const node = useRef<HTMLDivElement>(null)
  useEffect(() => {
    const ids = JSON.parse(scope) as string[]
    const controller = new AbortController()
    let timer: number | undefined
    async function refresh() {
      setRefreshing(true)
      let index = 0
      let pending = false
      await Promise.all(Array.from({ length: Math.min(3, ids.length) }, async () => {
        while (index < ids.length && !controller.signal.aborted) {
          const id = ids[index++]
          try {
            const value = await getNavMonitor(id, controller.signal)
            if (controller.signal.aborted) return
            if (value.source_id !== id) throw new Error('净值来源不匹配')
            pending ||= value.loading
            setEntries(previous => ({ ...previous, [id]: value.points.length === 0 && previous[id]?.value?.points.length
              ? { value: previous[id].value, stale: true, error: value.unavailable_reason ?? '历史更新中' }
              : { value } }))
          } catch (error) {
            if (controller.signal.aborted) return
            pending ||= error instanceof ApiError && error.navPricing?.loading === true
            setEntries(previous => ({ ...previous, [id]: { value: previous[id]?.value, stale: true, error: error instanceof Error ? error.message : String(error) } }))
          }
        }
      }))
      if (!controller.signal.aborted) {
        setRefreshing(false)
        if (autoRefresh) timer = window.setTimeout(() => void refresh(), pending ? 10_000 : 60_000)
      }
    }
    void refresh()
    return () => { controller.abort(); window.clearTimeout(timer) }
  }, [scope, revision, autoRefresh])

  useEffect(() => {
    if (!node.current) return
    const chart = echarts.init(node.current)
    const observer = new ResizeObserver(() => chart.resize())
    observer.observe(node.current)
    return () => { observer.disconnect(); chart.dispose() }
  }, [])
  useEffect(() => {
    const chart = node.current && echarts.getInstanceByDom(node.current)
    if (!chart) return
    chart.setOption({
      animation: false,
      useUTC: true,
      legend: { type: 'scroll', top: 4, data: accounts.map(a => ({ name: a.source_id, icon: 'roundRect' })), formatter: (id: string) => accounts.find(a => a.source_id === id)?.account ?? id },
      grid: { left: 80, right: 24, top: 48, bottom: 76 },
      tooltip: { trigger: 'axis', renderMode: 'richText', valueFormatter: (v: unknown) => typeof v === 'number' ? money(v) : '—' },
      xAxis: { type: 'time', axisLabel: { formatter: (v: number) => time.format(v) } },
      yAxis: { type: 'value', name: '累计变化（报价币）', scale: true, axisLabel: { formatter: (v: number) => money(v) } },
      dataZoom: [{ type: 'inside' }, { type: 'slider', bottom: 12 }],
      series: accounts.map(account => ({
        id: account.source_id, name: account.source_id, type: 'line', showSymbol: false, connectNulls: false,
        lineStyle: { width: 1.5, type: entries[account.source_id]?.stale ? 'dashed' : 'solid' },
        data: entries[account.source_id]?.value?.points.map(p => [p.ts_ms, p.pnl_quote]) ?? [],
        markLine: { silent: true, symbol: 'none', label: { show: false }, data: [{ yAxis: 0 }], lineStyle: { color: '#94a3b8', width: 1 } },
      })),
    }, { replaceMerge: ['series'] })
  }, [accounts, entries])

  return <section className="overview-panel mb-5" aria-labelledby="nav-monitor-title">
    <div className="overview-table-toolbar">
      <div><h2 id="nav-monitor-title" className="font-semibold">净值监控</h2><p className="mt-1 text-xs text-muted">从各盘监控历史起点归零，每 4 小时一个点，并保留起点与最新点。FR 不计浮动盈亏；资金费沿用各账户口径。</p></div>
      <Button type="button" onClick={() => setRevision(v => v + 1)} disabled={refreshing} aria-label="刷新净值"><RefreshCw size={14} />{refreshing ? '更新中' : '刷新净值'}</Button>
    </div>
    <div ref={node} className="h-[420px] w-full" role="img" aria-label="各账户运行以来累计净值变化曲线，时间为上海时间" />
    <div className="overview-table-scroll" tabIndex={0} role="region" aria-label="净值监控账户明细">
      <table className="overview-table"><thead><tr><th>账户</th><th>累计变化</th><th>历史起点（上海）</th><th>截至（上海）</th><th>口径与状态</th></tr></thead>
        <tbody>{accounts.map(account => {
          const entry = entries[account.source_id]
          const value = entry?.value
          const latest = value?.points.at(-1)?.pnl_quote
          const notice = entry?.error ?? value?.unavailable_reason
          return <tr key={account.source_id}>
            <th scope="row">{account.account}</th><td>{latest == null ? '—' : money(latest)}</td>
            <td>{value?.start_ms ? fullTime.format(value.start_ms) : '—'}</td><td>{value?.end_ms ? fullTime.format(value.end_ms) : '—'}</td>
            <td className="!whitespace-normal !min-w-64 text-xs">{value?.valuation === 'quantity_fifo_window_delta' ? '成交价估值 · ' : value?.valuation === 'quantity_fifo_closed_1m_window_delta' ? '分钟收盘价 · ' : account.account_kind === 'fr' ? '已实现收益 · ' : '含浮动盈亏 · '}{value?.funding === 'estimated' ? '估算资金费 · ' : value?.funding === 'actual' ? '实际资金费 · ' : ''}{notice ?? (value?.loading ? '历史加载中' : value?.complete ? '已更新' : '等待数据')}{entry?.stale ? '（保留上次结果）' : ''}</td>
          </tr>
        })}</tbody>
      </table>
      {accounts.length === 0 && <div className="overview-empty">没有匹配的账户</div>}
    </div>
    <p className="px-5 py-3 text-xs text-muted">曲线展示当前筛选账户的运行以来变化，不受上方日期选择影响。不同账户各自归零；金额按各自报价币计量，不作跨币种合计。成交价估值的浮动及总体净值为估计值。</p>
  </section>
}
