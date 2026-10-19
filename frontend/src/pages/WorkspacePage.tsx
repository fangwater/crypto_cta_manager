import { ArrowDown, ArrowDownUp, ArrowRight, ArrowUp, Check, ChevronDown, Clock3, Filter, LayoutDashboard, RefreshCw, Search, WalletCards, X } from 'lucide-react'
import { useMemo, useState } from 'react'
import { AppShell } from '../components/AppShell'
import { Alert } from '../components/ui/Badge'
import { Button } from '../components/ui/Button'
import { useWorkspaceData } from '../hooks/useWorkspaceData'
import { money } from '../format'
import { routes } from '../lib/routes'
import { accountMetrics, accountVenueLabel, workspaceAccounts } from '../lib/workspace'
import type { DashboardAccount, SourceNavReport } from '../types'
import './workspace.css'

type SortKey = 'account' | 'equity' | 'pnl' | 'long' | 'short' | 'longLeverage' | 'shortLeverage' | 'net'
const dateTime = new Intl.DateTimeFormat('zh-CN', { timeZone: 'Asia/Shanghai', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit', hourCycle: 'h23' })
const displayTime = (ms: number | null | undefined) => ms == null ? '—' : dateTime.format(ms)
const number = (value: number | null) => value === null ? '—' : money(value)
const multiple = (value: number | null) => value === null ? '—' : `${value.toFixed(3)}×`

export function WorkspacePage() {
  const { dashboard, health, error, refreshing, autoRefresh, setAutoRefresh, countdown, receivedAt, refresh } = useWorkspaceData()
  const [search, setSearch] = useState('')
  const [venue, setVenue] = useState('all')
  const [status, setStatus] = useState('all')
  const [filterOpen, setFilterOpen] = useState(false)
  const [hidden, setHidden] = useState<Set<string>>(new Set())
  const [sort, setSort] = useState<{ key: SortKey; descending: boolean }>({ key: 'account', descending: false })
  const accounts = useMemo(() => workspaceAccounts(dashboard), [dashboard])
  const reports = useMemo(() => new Map(dashboard?.report.sources.map(report => [report.source_id, report]) ?? []), [dashboard])
  const venues = [...new Set(accounts.map(account => account.venue))].sort()
  const rows = accounts.filter(account => {
    const query = search.trim().toLowerCase()
    return !hidden.has(account.source_id)
      && (venue === 'all' || account.venue === venue)
      && (status === 'all' || (status === 'enabled' ? account.enabled : !account.enabled))
      && (!query || `${account.account} ${account.source_id}`.toLowerCase().includes(query))
  }).map(account => ({ account, report: reports.get(account.source_id), metrics: accountMetrics(account, reports.get(account.source_id)) }))
  rows.sort((a, b) => {
    if (sort.key === 'account') return a.account.account.localeCompare(b.account.account, 'zh-CN', { numeric: true }) * (sort.descending ? -1 : 1)
    const av = a.metrics[sort.key], bv = b.metrics[sort.key]
    if (av === null) return bv === null ? 0 : 1
    if (bv === null) return -1
    return (av - bv) * (sort.descending ? -1 : 1)
  })
  const enabled = rows.filter(row => row.account.enabled).length
  const fresh = rows.filter(row => row.account.enabled && row.metrics.equity !== null && row.account.live_equity_status === 'ok').length
  const stale = rows.filter(row => row.account.enabled && row.account.live_equity_status === 'stale').length
  const waiting = rows.filter(row => row.account.enabled && (row.metrics.equity === null || !['ok', 'stale'].includes(row.account.live_equity_status ?? ''))).length
  const filtered = search !== '' || venue !== 'all' || status !== 'all' || hidden.size > 0
  const healthLabel = error ? '连接异常' : !health ? '检查中' : health.status === 'ok' ? '服务在线' : '数据延迟'

  function sortColumn(key: SortKey, label: string, unit?: string) {
    const selected = sort.key === key
    const Icon = selected ? sort.descending ? ArrowDown : ArrowUp : ArrowDownUp
    return <th scope="col" aria-sort={selected ? sort.descending ? 'descending' : 'ascending' : 'none'}>
      <button type="button" className="overview-sort" onClick={() => setSort({ key, descending: selected ? !sort.descending : key !== 'account' })}>
        <span>{label}{unit && <small>{unit}</small>}</span><Icon size={12} />
      </button>
    </th>
  }

  return <AppShell active="workspace" title="CTA 管理平台" subtitle="账户监控总览" icon={LayoutDashboard} wide className="overview-page">
    <div className="overview-heading">
      <div><p className="overview-eyebrow">账户监控</p><h2>总览<span>实时掌握每个账户的运行状态</span></h2></div>
      <div className="overview-heading-actions"><span className={`overview-service ${!error && health?.status === 'ok' ? 'is-online' : ''}`}><i />{healthLabel}</span><a href={routes.accounts}>账户管理 <ArrowRight size={15} /></a></div>
    </div>

    <section className="overview-toolbar" aria-label="总览筛选与统计">
      <div className="overview-filters">
        <label>账户类型<select value={venue} onChange={event => setVenue(event.target.value)}><option value="all">全部类型</option>{venues.map(value => <option key={value} value={value}>{accountVenueLabel(value)}</option>)}</select></label>
        <label>账户状态<select value={status} onChange={event => setStatus(event.target.value)}><option value="all">全部账户</option><option value="enabled">已启用</option><option value="disabled">未启用</option></select></label>
        <label className="overview-auto"><input type="checkbox" checked={autoRefresh} onChange={event => setAutoRefresh(event.target.checked)} />自动刷新<span>{refreshing ? '更新中' : autoRefresh ? `${countdown} 秒` : '已暂停'}</span></label>
      </div>
      <div className="overview-stats" aria-live="polite">
        <Summary label="当前账户" value={dashboard ? rows.length : null} detail={dashboard ? `共 ${accounts.length} 个` : '正在载入'} />
        <Summary label="已启用" value={dashboard ? enabled : null} detail="独立运行账户" />
        <Summary label="权益正常" value={dashboard ? fresh : null} detail="实时数据可用" tone="good" />
        <Summary label="权益延迟" value={dashboard ? stale : null} detail={dashboard ? `${waiting} 个等待数据` : '正在检查'} tone={stale ? 'warn' : ''} />
      </div>
    </section>

    {error && <Alert tone="error" className="mb-4">更新失败：{error}。{dashboard ? '当前保留上次数据，请刷新重试。' : '请刷新重试。'}</Alert>}
    {health?.last_refresh_error && <Alert tone="warning" className="mb-4">报表更新暂时失败，当前展示上一份有效数据。</Alert>}

    <section className="overview-panel" aria-labelledby="overview-table-title">
      <div className="overview-table-toolbar">
        <div className="overview-table-title"><h3 id="overview-table-title">账户数据</h3><span>{dashboard ? `${rows.length} 个账户` : '正在载入'}</span><span className="overview-latest">最新快照</span></div>
        <div className="overview-table-actions">
          <label className="overview-search"><Search size={16} /><input type="search" aria-label="搜索账户" placeholder="搜索账户名称或标识" value={search} onChange={event => setSearch(event.target.value)} /></label>
          <Button type="button" onClick={() => setFilterOpen(value => !value)} aria-expanded={filterOpen} aria-controls="overview-account-filter"><Filter size={14} />账户筛选{hidden.size > 0 && <span>({hidden.size})</span>}<ChevronDown size={13} /></Button>
          <Button type="button" onClick={() => void refresh()} disabled={refreshing} aria-label="刷新数据"><RefreshCw size={14} className={refreshing ? 'animate-spin-slow' : ''} /><span className="hidden sm:inline">刷新</span></Button>
        </div>
      </div>
      {filterOpen && <div id="overview-account-filter" className="overview-account-filter">
        <div><strong>选择展示的账户</strong><button type="button" onClick={() => setHidden(new Set())}>全选</button><button type="button" onClick={() => setHidden(new Set(accounts.map(account => account.source_id)))}>全不选</button><button type="button" aria-label="收起账户筛选" onClick={() => setFilterOpen(false)}><X size={16} /></button></div>
        <fieldset><legend className="sr-only">展示账户</legend>{accounts.map(account => <label key={account.source_id}><input type="checkbox" checked={!hidden.has(account.source_id)} onChange={() => setHidden(previous => { const next = new Set(previous); if (next.has(account.source_id)) next.delete(account.source_id); else next.add(account.source_id); return next })} />{account.account}</label>)}</fieldset>
      </div>}
      <p className="overview-scroll-hint">左右滑动表格，查看更多指标</p>
      <div className="overview-table-scroll" tabIndex={0} role="region" aria-label="账户监控数据表，可横向滚动" aria-busy={refreshing}>
        <table className="overview-table">
          <thead><tr><th scope="col" className="overview-index">序号</th>{sortColumn('account', '账户')}<th scope="col">权益状态</th>{sortColumn('equity', '实时权益', 'USDT')}{sortColumn('pnl', '累计费后净值', '报价币')}{sortColumn('long', '多头持仓', '报价币')}{sortColumn('short', '空头持仓', '报价币')}{sortColumn('longLeverage', '多头杠杆', '估算倍数')}{sortColumn('shortLeverage', '空头杠杆', '估算倍数')}{sortColumn('net', '净敞口', '报价币')}<th scope="col">最近成交<small>上海时间</small></th><th scope="col">查看</th></tr></thead>
          <tbody>
            {!dashboard && !error ? Array.from({ length: 6 }, (_, index) => <tr key={index} className="overview-skeleton" aria-hidden="true">{Array.from({ length: 12 }, (_, col) => <td key={col}><span /></td>)}</tr>) : rows.map(({ account, report, metrics }, index) => <AccountRow key={account.source_id} account={account} report={report} metrics={metrics} index={index} />)}
          </tbody>
        </table>
        {(!dashboard && error || dashboard && rows.length === 0) && <div className="overview-empty"><WalletCards size={32} /><h4>{error && !dashboard ? '暂时无法加载账户' : accounts.length === 0 ? '暂无可查看账户' : '没有匹配的账户'}</h4><p>{accounts.length === 0 ? '已授权账户将在这里展示。' : '调整筛选条件，或清除搜索后重试。'}</p>{filtered && <Button type="button" onClick={() => { setSearch(''); setVenue('all'); setStatus('all'); setHidden(new Set()) }}>清除筛选</Button>}</div>}
      </div>
      <div className="overview-table-footer"><span><Check size={13} />账户独立展示 · 持仓按最近成交价估值</span><span><Clock3 size={13} />最近获取 {displayTime(receivedAt)}<span className="overview-footer-divider">/</span>报表时间 {displayTime(dashboard ? dashboard.generated_at_us / 1000 : null)}</span></div>
    </section>
    <div className="overview-notes"><p>累计费后净值为快照起点以来的订单损益，已扣估算交易手续费，不含资金费。多空杠杆以持仓名义金额除以实时权益估算；混合 USDT / USDC 按等值估算，权益延迟或币本位账户不显示杠杆。</p><p>“—”表示数据暂不可用。当前提供最新快照；昨日权益、今日盈亏、回撤与保证金尚未接入。</p></div>
  </AppShell>
}

function Summary({ label, value, detail, tone = '' }: { label: string; value: number | null; detail: string; tone?: string }) {
  return <div className={`overview-stat ${tone}`}><p>{label}</p><strong>{value ?? '—'}</strong><small>{detail}</small></div>
}

function AccountRow({ account, report, metrics, index }: { account: DashboardAccount; report?: SourceNavReport; metrics: ReturnType<typeof accountMetrics>; index: number }) {
  const status = !account.enabled ? '未启用' : metrics.equity === null ? '等待数据' : account.live_equity_status === 'ok' ? '正常' : account.live_equity_status === 'stale' ? '延迟' : '等待数据'
  const tone = status === '正常' ? 'good' : status === '延迟' ? 'warn' : 'neutral'
  return <tr>
    <td className="overview-index">{String(index + 1).padStart(2, '0')}</td>
    <th scope="row" className="overview-account-cell"><div className="overview-account-name"><a href={routes.account(account.source_id)}>{account.account}</a><span className={`overview-venue ${account.venue === 'binance-coin-futures' ? 'coin' : ''}`}>{accountVenueLabel(account.venue)}</span></div><small title={account.source_id}>{account.source_id}</small></th>
    <td><span className={`overview-status ${tone}`}><i />{status}</span></td>
    <td className={account.live_equity_status === 'stale' ? 'overview-stale' : 'overview-equity'}>{number(metrics.equity)}</td>
    <td className={metrics.pnl !== null ? metrics.pnl > 0 ? 'overview-positive' : metrics.pnl < 0 ? 'overview-negative' : '' : ''}>{metrics.pnl !== null && metrics.pnl > 0 ? '+' : ''}{number(metrics.pnl)}</td>
    <td>{number(metrics.long)}</td><td>{number(metrics.short)}</td><td>{multiple(metrics.longLeverage)}</td><td>{multiple(metrics.shortLeverage)}</td><td>{number(metrics.net)}</td>
    <td className="overview-time">{displayTime(report?.last_fill_ts_us == null ? null : report.last_fill_ts_us / 1000)}</td>
    <td>{account.enabled && report ? <a className="overview-detail" href={routes.nav(account.source_id)} aria-label={`查看 ${account.account} 净值`}>净值 <ArrowRight size={13} /></a> : <span className="overview-muted">—</span>}</td>
  </tr>
}
