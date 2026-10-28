import { Activity, Search } from 'lucide-react'
import { useState } from 'react'
import { AppShell } from '../components/AppShell'
import { OverviewNavMonitor } from '../components/OverviewNavMonitor'
import { Alert } from '../components/ui/Badge'
import { useAccountOverview } from '../hooks/useAccountOverview'
import { accountVenueLabel } from '../lib/workspace'
import './workspace.css'

export function NavMonitorPage() {
  const { data, error, autoRefresh, setAutoRefresh } = useAccountOverview('')
  const [search, setSearch] = useState('')
  const [kind, setKind] = useState('all')
  const [venue, setVenue] = useState('all')
  const accounts = data?.accounts ?? []
  const venues = [...new Set(accounts.map(account => account.venue))].sort()
  const rows = accounts.filter(account => (kind === 'all' || (account.account_kind || 'cta') === kind)
    && (venue === 'all' || account.venue === venue)
    && `${account.account} ${account.source_id}`.toLowerCase().includes(search.trim().toLowerCase()))
  return <AppShell active="nav-monitor" title="净值" subtitle="运行以来 · 分账户累计变化" icon={Activity} wide className="overview-page">
    <section className="overview-toolbar mb-5" aria-label="净值账户筛选">
      <div className="overview-filters">
        <label>账户类别<select aria-label="账户类别" value={kind} onChange={event => setKind(event.target.value)}><option value="all">全部账户</option><option value="cta">CTA</option><option value="fr">FR</option><option value="mm">MM</option></select></label>
        <label>账户类型<select aria-label="账户类型" value={venue} onChange={event => setVenue(event.target.value)}><option value="all">全部类型</option>{venues.map(value => <option key={value} value={value}>{accountVenueLabel(value)}</option>)}</select></label>
        <label className="overview-search"><Search size={16} /><input type="search" aria-label="搜索账户" placeholder="搜索账户" value={search} onChange={event => setSearch(event.target.value)} /></label>
        <label className="overview-auto"><input type="checkbox" checked={autoRefresh} onChange={event => setAutoRefresh(event.target.checked)} />自动刷新</label>
      </div>
      <span className="text-sm text-muted">{data ? `${rows.length} 个账户` : '账户加载中'}</span>
    </section>
    {error && <Alert tone="error" className="mb-4">账户列表更新失败：{error}</Alert>}
    <OverviewNavMonitor accounts={rows} autoRefresh={autoRefresh} />
  </AppShell>
}
