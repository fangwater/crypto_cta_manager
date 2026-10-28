import { useEffect, useRef, useState } from 'react'
import * as echarts from 'echarts/core'
import { LineChart } from 'echarts/charts'
import { DataZoomComponent, GridComponent, MarkLineComponent, TooltipComponent } from 'echarts/components'
import { CanvasRenderer } from 'echarts/renderers'
import { ArrowDownRight, ArrowUpRight, RefreshCw } from 'lucide-react'
import { ApiError, getNavMonitor, type NavMonitor } from '../api'
import { money } from '../format'
import { isExternalOverview, type OverviewRow } from '../lib/accountOverview'
import { accountVenueLabel } from '../lib/workspace'
import { routes } from '../lib/routes'
import { Badge } from './ui/Badge'
import { Button } from './ui/Button'

echarts.use([LineChart, DataZoomComponent, GridComponent, MarkLineComponent, TooltipComponent, CanvasRenderer])
const time = new Intl.DateTimeFormat('zh-CN', { timeZone: 'Asia/Shanghai', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', hourCycle: 'h23' })
const fullTime = new Intl.DateTimeFormat('zh-CN', { timeZone: 'Asia/Shanghai', year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', hourCycle: 'h23' })
type Entry = { value?: NavMonitor; error?: string; stale?: boolean }

export function OverviewNavMonitor({ accounts, autoRefresh }: { accounts: OverviewRow[]; autoRefresh: boolean }) {
  const [entries, setEntries] = useState<Record<string, Entry>>({})
  const [revision, setRevision] = useState(0)
  const [refreshing, setRefreshing] = useState(false)
  const scope = JSON.stringify(accounts.map(a => a.source_id).sort())
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

  return <section className="overview-nav-monitor mb-5" aria-labelledby="nav-monitor-title">
    <div className="overview-table-toolbar !px-0">
      <div><h2 id="nav-monitor-title" className="font-semibold">分账户净值监控</h2><p className="mt-1 text-xs text-muted">每个账户独立展示监控以来的累计变化。上海时间 00、04、08、12、16、20 点计算并保存。</p></div>
      <Button type="button" onClick={() => setRevision(v => v + 1)} disabled={refreshing} aria-label="刷新净值"><RefreshCw size={14} />{refreshing ? '更新中' : '刷新净值'}</Button>
    </div>
    <div className="overview-nav-grid">
      {accounts.map(account => <AccountNavCard key={account.source_id} account={account} entry={entries[account.source_id]} />)}
    </div>
    {accounts.length === 0 && <div className="overview-panel overview-empty">没有匹配的账户</div>}
    <p className="py-3 text-xs text-muted">各账户从自己的监控历史起点归零。曲线读取已保存的四小时整点结果，截至最近一期；时间均为上海时间。</p>
  </section>
}

function AccountNavCard({ account, entry }: { account: OverviewRow; entry?: Entry }) {
  const value = entry?.value
  const latest = value?.points.at(-1)?.pnl_quote
  const previous = value?.points.at(-2)?.pnl_quote
  const change = latest != null && previous != null ? latest - previous : null
  const TrendIcon = (latest ?? 0) < 0 ? ArrowDownRight : ArrowUpRight
  const notice = entry?.error ?? value?.unavailable_reason
  const loading = !entry || value?.loading
  const hasPoints = Boolean(value?.points.length)
  const status = entry?.stale ? '更新失败' : loading ? '加载中' : value?.complete ? '已更新' : hasPoints ? '部分数据' : '暂不可用'
  const valuation = value?.valuation === 'quantity_fifo_window_delta' ? '成交价估值（浮动及总体为估计值）'
    : value?.valuation === 'quantity_fifo_closed_1m_window_delta' ? '分钟收盘价估值'
    : account.account_kind === 'fr' ? '已实现收益，不计浮动盈亏' : '含浮动盈亏'
  const funding = value?.funding === 'estimated' ? '估算资金费' : value?.funding === 'actual' ? '实际资金费' : undefined
  const href = isExternalOverview(account) ? account.detail_url ?? undefined : routes.nav(account.source_id)
  return <article className="overview-panel overview-nav-card" data-nav-source={account.source_id} aria-label={`${account.account} 净值监控`}>
    <header className="overview-nav-card-header">
      <div className="min-w-0">
        <div className="overview-nav-identity"><span className={`overview-nav-kind kind-${account.account_kind || 'cta'}`}>{(account.account_kind || 'cta').toUpperCase()}</span><span className="text-xs text-muted">{accountVenueLabel(account.venue)}</span></div>
        <h3 className="mt-2 break-words text-sm font-semibold text-ink"><a className="hover:text-brand" href={href}>{account.account}</a></h3>
      </div>
      <Badge tone={entry?.stale || notice ? 'warning' : value?.complete ? 'success' : 'neutral'}>{status}</Badge>
    </header>
    <div className="overview-nav-metrics">
      <div>
        <p className="overview-nav-metric-label">累计净值变化 <span>报价币</span></p>
        <p className={`overview-nav-number ${latest == null || latest === 0 ? 'text-ink' : latest > 0 ? 'text-teal-700' : 'text-rose-600'}`} data-nav-value>{latest == null ? '—' : `${latest > 0 ? '+' : ''}${money(latest)}`}<TrendIcon size={20} aria-hidden="true" /></p>
      </div>
      <div className="overview-nav-change"><span>本期变化</span><strong className={change == null || change === 0 ? 'text-muted' : change > 0 ? 'text-teal-700' : 'text-rose-600'}>{change == null ? '—' : `${change > 0 ? '+' : ''}${money(change)}`}</strong></div>
    </div>
    <div className="relative">
      <AccountNavChart account={account} entry={entry} />
      {!hasPoints && <div className="absolute inset-0 flex items-center justify-center px-8 text-center text-xs text-muted" role="status">{notice ?? (loading ? '正在加载此账户历史' : '此账户暂无可用历史')}</div>}
    </div>
    <footer className="overview-nav-card-footer">
      <div className="overview-nav-period"><span>起点 <time>{value?.start_ms ? fullTime.format(value.start_ms) : '—'}</time></span><span>截至 <time>{value?.end_ms ? fullTime.format(value.end_ms) : '—'}</time></span></div>
      <div className="overview-nav-method"><span className="overview-nav-dot" /><span>{valuation}{funding ? ` · ${funding}` : ''}</span></div>
      {notice && hasPoints && <p className="text-amber-700" role="status">{notice}{entry?.stale ? '（保留上次结果）' : ''}</p>}
    </footer>
  </article>
}

function AccountNavChart({ account, entry }: { account: OverviewRow; entry?: Entry }) {
  const node = useRef<HTMLDivElement>(null)
  const chart = useRef<ReturnType<typeof echarts.init> | null>(null)
  useEffect(() => {
    if (!node.current) return
    const instance = echarts.init(node.current)
    chart.current = instance
    const observer = new ResizeObserver(() => instance.resize())
    observer.observe(node.current)
    return () => { observer.disconnect(); instance.dispose(); chart.current = null }
  }, [])
  useEffect(() => {
    if (!chart.current) return
    const latest = entry?.value?.points.at(-1)?.pnl_quote ?? 0
    const color = latest < 0 ? '#e05271' : '#0d9488'
    const tint = latest < 0 ? '224, 82, 113' : '13, 148, 136'
    chart.current.setOption({
      animation: false,
      useUTC: true,
      textStyle: { fontFamily: 'Inter, system-ui, sans-serif', color: '#64748b' },
      grid: { left: 20, right: 24, top: 20, bottom: 58, containLabel: true },
      tooltip: {
        trigger: 'axis', renderMode: 'richText', confine: true, padding: [12, 16],
        backgroundColor: '#fff', borderColor: '#e4eaf1', borderWidth: 1,
        axisPointer: { type: 'line', lineStyle: { color: '#94a3b8', type: 'dashed' } },
        formatter: (raw: unknown) => {
          const point = (Array.isArray(raw) ? raw[0] : raw) as { value?: [number, number] }
          if (!point?.value) return ''
          return `{date|${fullTime.format(point.value[0])}}\n{label|累计变化}   {amount|${point.value[1] > 0 ? '+' : ''}${money(point.value[1])}}`
        },
        textStyle: { rich: {
          date: { fontSize: 11, color: '#64748b', lineHeight: 24 },
          label: { fontSize: 12, color: '#334155', lineHeight: 24 },
          amount: { fontSize: 15, fontWeight: 600, color, fontFamily: 'monospace' },
        } },
      },
      xAxis: {
        type: 'time', splitNumber: 3, axisLine: { show: false }, axisTick: { show: false },
        axisLabel: { formatter: (v: number) => time.format(v), hideOverlap: true, color: '#94a3b8', fontSize: 10, margin: 14 },
        axisPointer: { label: { show: false } },
      },
      yAxis: {
        type: 'value', scale: true, splitNumber: 4, axisLine: { show: false }, axisTick: { show: false },
        axisLabel: { formatter: (v: number) => Math.abs(v) >= 10_000 ? `${+(v / 10_000).toFixed(1)}万` : money(v), color: '#94a3b8', fontSize: 10, margin: 12 },
        splitLine: { lineStyle: { color: '#e9eef4', type: 'dashed' } },
      },
      dataZoom: [
        { type: 'inside', minValueSpan: 4 * 3_600_000 },
        { type: 'slider', bottom: 8, left: 30, right: 24, height: 16, showDetail: false,
          borderColor: '#e8edf3', backgroundColor: '#f8fafc', fillerColor: `rgba(${tint}, .07)`,
          handleSize: '130%', handleStyle: { color: '#fff', borderColor: color, borderWidth: 1 },
          moveHandleSize: 0, brushSelect: false,
          dataBackground: { lineStyle: { color: '#b8c7d7' }, areaStyle: { color: '#e2e8f0' } },
          selectedDataBackground: { lineStyle: { color }, areaStyle: { color: `rgba(${tint}, .12)` } },
        },
      ],
      series: [{
        id: account.source_id, name: account.account, type: 'line', showSymbol: false, connectNulls: false,
        lineStyle: { width: 2.4, color, type: entry?.stale ? 'dashed' : 'solid' },
        itemStyle: { color, borderColor: '#fff', borderWidth: 2 }, symbol: 'circle', symbolSize: 7,
        areaStyle: { origin: 0, color: new echarts.graphic.LinearGradient(0, 0, 0, 1, [
          { offset: 0, color: `rgba(${tint}, .18)` }, { offset: 1, color: `rgba(${tint}, .01)` },
        ]) },
        emphasis: { lineStyle: { width: 2.8 } },
        data: entry?.value?.points.map(p => [p.ts_ms, p.pnl_quote]) ?? [],
        markLine: { silent: true, symbol: 'none', label: { show: false }, data: [{ yAxis: 0 }], lineStyle: { color: '#b8c7d7', width: 1, type: 'dashed' } },
      }],
    }, { replaceMerge: ['series'] })
  }, [account.source_id, account.account, entry?.value, entry?.stale])
  return <div ref={node} className="overview-nav-chart" data-nav-chart={account.source_id} role="img" aria-label={`${account.account} 独立累计净值变化曲线，时间为上海时间`} />
}
