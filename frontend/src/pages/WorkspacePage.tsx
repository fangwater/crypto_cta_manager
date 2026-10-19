import { ArrowDown, ArrowDownUp, ArrowUp, ChevronDown, Filter, LayoutDashboard, RefreshCw, Search, X } from 'lucide-react'
import { useState } from 'react'
import { AppShell } from '../components/AppShell'
import { useAuth } from '../components/AuthGate'
import { Alert } from '../components/ui/Badge'
import { Button } from '../components/ui/Button'
import { useAccountOverview } from '../hooks/useAccountOverview'
import { money } from '../format'
import { routes } from '../lib/routes'
import { accountVenueLabel } from '../lib/workspace'
import { overviewAlertCount, overviewTotal, type OverviewRow } from '../lib/accountOverview'
import './workspace.css'

type ValueKey = 'yesterday_equity' | 'equity' | 'today_profit' | 'profit_percent' | 'long_position' | 'short_position' | 'long_leverage' | 'short_leverage' | 'net_position' | 'max_drawdown_15d_percent' | 'margin' | 'margin_percent' | 'date_ms' | 'queried_at_ms'
type SortKey = 'account' | ValueKey
const columns: { key: ValueKey; label: string; digits?: number; reason: string }[] = [
  { key: 'yesterday_equity', label: '昨日权益', reason: '缺少前一日的历史权益记录' },
  { key: 'equity', label: '权益', reason: '尚未收到账户权益' },
  { key: 'today_profit', label: '今日盈利', reason: '缺少当日或昨日权益，无法计算今日盈利' },
  { key: 'profit_percent', label: '盈亏比例(%)', digits: 4, reason: '缺少权益记录，或昨日权益不大于零' },
  { key: 'long_position', label: '多头持仓', reason: '尚未收到有效的实时多头持仓' },
  { key: 'short_position', label: '空头持仓', reason: '尚未收到有效的实时空头持仓' },
  { key: 'long_leverage', label: '多头杠杆', digits: 4, reason: '缺少有效持仓或正权益' },
  { key: 'short_leverage', label: '空头杠杆', digits: 4, reason: '缺少有效持仓或正权益' },
  { key: 'net_position', label: '轧差', reason: '缺少有效的多空持仓' },
  { key: 'max_drawdown_15d_percent', label: '15日最大回撤(%)', digits: 4, reason: '历史权益记录尚未覆盖15日' },
  { key: 'margin', label: '保证金', reason: '尚未收到交易所初始保证金' },
  { key: 'margin_percent', label: '保证金占比(%)', digits: 4, reason: '缺少保证金或正权益' },
  { key: 'date_ms', label: '日期', reason: '尚未收到权益数据时间' },
  { key: 'queried_at_ms', label: '查询时间', reason: '尚未查询' },
]
const dateTime = new Intl.DateTimeFormat('zh-CN', { timeZone: 'Asia/Shanghai', year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit', hourCycle: 'h23' })
const number = (value: number | null) => value === null ? '—' : money(value)

export function WorkspacePage() {
  const [date, setDate] = useState('')
  const { data, error, refreshing, autoRefresh, setAutoRefresh, countdown, refresh } = useAccountOverview(date)
  const { user } = useAuth()
  const [search, setSearch] = useState('')
  const [venue, setVenue] = useState('all')
  const [filterOpen, setFilterOpen] = useState(false)
  const [hidden, setHidden] = useState<Set<string>>(new Set())
  const [sort, setSort] = useState<{ key: SortKey; descending: boolean }>({ key: 'account', descending: false })
  const accounts = data?.accounts ?? []
  const venues = [...new Set(accounts.map(account => account.venue))].sort()
  const rows = accounts.filter(account => !hidden.has(account.source_id)
    && (venue === 'all' || account.venue === venue)
    && `${account.account} ${account.source_id}`.toLowerCase().includes(search.trim().toLowerCase()))
    .sort((a, b) => {
      const av = a[sort.key], bv = b[sort.key]
      if (av == null) return bv == null ? 0 : 1
      if (bv == null) return -1
      const comparison = typeof av === 'string' && typeof bv === 'string' ? av.localeCompare(bv, 'zh-CN', { numeric: true }) : Number(av) - Number(bv)
      return sort.descending ? -comparison : comparison
    })
  function sortColumn(key: SortKey, label: string) {
    const active = sort.key === key
    const Icon = active ? sort.descending ? ArrowDown : ArrowUp : ArrowDownUp
    return <th scope="col" key={key} aria-sort={active ? sort.descending ? 'descending' : 'ascending' : 'none'}><button type="button" className="overview-sort" onClick={() => setSort({ key, descending: active ? !sort.descending : key !== 'account' })}>{label}<Icon size={12} /></button></th>
  }
  const leverageAlerts = data ? overviewAlertCount(rows, 'leverage_alert') : null
  const timeoutAlerts = data ? overviewAlertCount(rows, 'timeout_alert') : null
  return <AppShell active="workspace" title="CTA 管理平台" subtitle="账户监控总览" icon={LayoutDashboard} wide className="overview-page">
    <section className="overview-toolbar" aria-label="总览筛选与统计">
      <div className="overview-filters">
        <label>账户类型<select aria-label="账户类型" value={venue} onChange={event => setVenue(event.target.value)}><option value="all">全部类型</option>{venues.map(value => <option key={value} value={value}>{accountVenueLabel(value)}</option>)}</select></label>
        <label>日期<select aria-label="日期" value={date} onChange={event => setDate(event.target.value)}><option value="">最新</option>{[...new Set([...(date ? [date] : []), ...(data?.dates ?? [])])].sort().reverse().map(value => <option key={value} value={value}>{value}</option>)}</select></label>
        <label className="overview-auto"><input type="checkbox" checked={autoRefresh} onChange={event => setAutoRefresh(event.target.checked)} />自动刷新<span>{refreshing ? '更新中' : autoRefresh ? `${countdown} 秒` : '已暂停'}</span></label>
      </div>
      <div className="overview-stats" aria-live="polite">
        <Summary label="账户数" value={data ? String(rows.length) : '—'} />
        <Summary label="总权益" value={number(data ? overviewTotal(rows, 'equity') : null)} title="当前筛选账户的权益合计；有账户缺失数据时不显示不完整合计" />
        <Summary label="今日总盈利" value={number(data ? overviewTotal(rows, 'today_profit') : null)} title="当前筛选账户的今日盈利合计；需要完整的昨日权益记录" />
        <Summary label="杠杆告警" value={leverageAlerts == null ? '—' : String(leverageAlerts)} title={data?.alerts.long_leverage_limit == null || data?.alerts.short_leverage_limit == null ? '尚未提供原页面的杠杆告警阈值' : `多头 > ${data.alerts.long_leverage_limit}，空头 > ${data.alerts.short_leverage_limit}`} warn={!!leverageAlerts} />
        <Summary label="超时告警" value={timeoutAlerts == null ? '—' : String(timeoutAlerts)} title={data?.alerts.timeout_secs == null ? '尚未提供原页面的超时告警阈值' : `权益数据超过 ${data.alerts.timeout_secs} 秒未更新`} warn={!!timeoutAlerts} />
      </div>
    </section>
    {error && <Alert tone="error" className="mb-4">更新失败：{error}。{data ? '当前保留上次数据。' : '请刷新重试。'}</Alert>}
    <section className="overview-panel" aria-labelledby="overview-table-title">
      <div className="overview-table-toolbar">
        <div className="overview-table-title"><h2 id="overview-table-title">账户监控</h2><span>{user.username}</span></div>
        <div className="overview-table-actions">
          <label className="overview-search"><Search size={16} /><input type="search" aria-label="搜索账户" placeholder="搜索账户" value={search} onChange={event => setSearch(event.target.value)} /></label>
          <Button type="button" onClick={() => setFilterOpen(value => !value)} aria-expanded={filterOpen} aria-controls="overview-account-filter"><Filter size={14} />账户筛选<ChevronDown size={13} /></Button>
          <Button type="button" onClick={() => void refresh()} disabled={refreshing} aria-label="刷新数据"><RefreshCw size={14} /></Button>
        </div>
      </div>
      {filterOpen && <div id="overview-account-filter" className="overview-account-filter">
        <div><strong>选择展示的账户</strong><button type="button" onClick={() => setHidden(new Set())}>全选</button><button type="button" onClick={() => setHidden(new Set(accounts.map(account => account.source_id)))}>全不选</button><button type="button" aria-label="收起账户筛选" onClick={() => setFilterOpen(false)}><X size={16} /></button></div>
        <fieldset><legend className="sr-only">展示账户</legend>{accounts.map(account => <label key={account.source_id}><input type="checkbox" checked={!hidden.has(account.source_id)} onChange={() => setHidden(previous => { const next = new Set(previous); if (next.has(account.source_id)) next.delete(account.source_id); else next.add(account.source_id); return next })} />{account.account}</label>)}</fieldset>
      </div>}
      <div className="overview-table-scroll" tabIndex={0} role="region" aria-label="账户监控数据表，可横向滚动" aria-busy={refreshing || data?.loading}>
        <table className="overview-table">
          <thead><tr><th scope="col" className="overview-index">#</th>{sortColumn('account', '账号')}{columns.map(column => sortColumn(column.key, column.label))}</tr></thead>
          <tbody>{!data && !error ? Array.from({ length: 6 }, (_, index) => <tr key={index} className="overview-skeleton" aria-hidden="true">{Array.from({ length: 16 }, (_, col) => <td key={col}><span /></td>)}</tr>) : rows.map((row, index) => <AccountRow key={row.source_id} row={row} index={index} />)}</tbody>
        </table>
        {data && rows.length === 0 && <div className="overview-empty">没有匹配的账户</div>}
      </div>
    </section>
  </AppShell>
}

function Summary({ label, value, title, warn }: { label: string; value: string; title?: string; warn?: boolean }) {
  return <div className={`overview-stat ${warn ? 'warn' : ''}`} title={title}><p>{label}</p><strong>{value}</strong></div>
}

function AccountRow({ row, index }: { row: OverviewRow; index: number }) {
  return <tr data-source={row.source_id}>
    <td className="overview-index">{index + 1}</td>
    <th scope="row" className="overview-account-cell"><a href={routes.account(row.source_id)} title={row.source_id}>{row.account}</a></th>
    {columns.map(column => {
      const value = row[column.key]
      const time = column.key === 'date_ms' || column.key === 'queried_at_ms'
      const profit = column.key === 'today_profit' || column.key === 'profit_percent'
      const alert = column.key === 'date_ms' && row.timeout_alert || (column.key === 'long_leverage' || column.key === 'short_leverage') && row.leverage_alert
      const className = alert ? 'overview-stale' : profit && value != null && value !== 0 ? value > 0 ? 'overview-positive' : 'overview-negative' : time ? 'overview-time' : column.key === 'equity' ? 'overview-equity' : ''
      return <td key={column.key} className={className} title={value == null ? column.reason : undefined}>{value == null ? '—' : time ? dateTime.format(value) : column.digits != null ? value.toFixed(column.digits) : money(value)}</td>
    })}
  </tr>
}
