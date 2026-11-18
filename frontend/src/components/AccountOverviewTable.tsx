import { ChevronDown, Filter, RefreshCw, Search, X } from 'lucide-react'
import { useState, type Dispatch, type ReactNode, type SetStateAction } from 'react'
import { useAuth } from './AuthGate'
import { Button } from './ui/Button'
import '../pages/workspace.css'

export function AccountOverviewTable({
  accounts, search, setSearch, hidden, setHidden, refresh, refreshing, loading, empty, headings, children,
}: {
  accounts: { source_id: string; account: string }[]
  search: string
  setSearch: (value: string) => void
  hidden: Set<string>
  setHidden: Dispatch<SetStateAction<Set<string>>>
  refresh: () => void
  refreshing: boolean
  loading?: boolean
  empty: boolean
  headings: ReactNode
  children: ReactNode
}) {
  const { user } = useAuth()
  const [filterOpen, setFilterOpen] = useState(false)

  return <section className="overview-panel" aria-labelledby="overview-table-title">
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
    <div className="overview-table-scroll" tabIndex={0} role="region" aria-label="账户监控数据表，可横向滚动" aria-busy={refreshing || loading}>
      <table className="overview-table">
        <thead><tr>{headings}</tr></thead>
        <tbody>{children}</tbody>
      </table>
      {empty && <div className="overview-empty">没有匹配的账户</div>}
    </div>
  </section>
}

export function OverviewStat({ label, value, title, warn }: { label: string; value: string; title?: string; warn?: boolean }) {
  return <div className={`overview-stat ${warn ? 'warn' : ''}`} title={title}><p>{label}</p><strong>{value}</strong></div>
}
