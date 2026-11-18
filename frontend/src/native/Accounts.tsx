import { Activity, LayoutDashboard, RefreshCw, Scale, WalletCards } from 'lucide-react'
import { useEffect, useState } from 'react'
import { ApiError } from '../api'
import { AccountOverviewTable, OverviewStat } from '../components/AccountOverviewTable'
import { AccountActionLink, AccountCardLayout, AccountMetric } from '../components/AccountCardLayout'
import { AppShell, PageIntro, StatTile } from '../components/AppShell'
import { Alert, Badge } from '../components/ui/Badge'
import { Button } from '../components/ui/Button'
import { routes } from '../lib/routes'
import * as api from './api'

const POLL_MS = 15_000

type AccountData = { snapshot?: api.Snapshot; error?: string; applying?: boolean }
export type AccountsData = {
  sources: api.Source[] | null
  accounts: Record<string, AccountData>
  error: string | null
  loading: boolean
  autoRefresh: boolean
  setAutoRefresh: (value: boolean) => void
  refresh: () => void
}

export const errorMessage = (error: unknown) => error instanceof Error ? error.message : String(error)
export const time = (us: number | null | undefined) => us == null ? '—' : new Date(us / 1000).toLocaleString('zh-CN', { timeZone: 'Asia/Shanghai', hour12: false })
export const money = (value: number | null | undefined) => value == null ? '—' : value.toLocaleString('zh-CN', { maximumFractionDigits: 2 })

export const accountType = (source: api.Source) => {
  if (source.adapter === 'yida') return '易达仿真'
  if (source.adapter === 'ctp') return 'CTP 实盘'
  return source.adapter
}

export function useNativeAccounts(selected?: string, catalogOnly = false): AccountsData {
  const [sources, setSources] = useState<api.Source[] | null>(null)
  const [accounts, setAccounts] = useState<Record<string, AccountData>>({})
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)
  const [autoRefresh, setAutoRefresh] = useState(true)
  const [revision, setRevision] = useState(0)

  useEffect(() => {
    const controller = new AbortController()
    const { signal } = controller
    let timer: ReturnType<typeof setTimeout> | undefined

    async function load() {
      setLoading(true)
      try {
        const rows = await api.getSources(signal)
        if (signal.aborted) return
        setSources(rows)
        setError(null)
        const ids = new Set(rows.map(row => row.source_id))
        setAccounts(previous => Object.fromEntries(Object.entries(previous).filter(([id]) => ids.has(id))))
        const current = rows.find(row => row.source_id === selected) ?? rows[0]
        const requested = catalogOnly ? [] : selected === undefined ? rows : current ? [current] : []
        await Promise.all(requested.map(async ({ source_id: id }) => {
          try {
            const snapshot = await api.getSnapshot(id, signal)
            if (!signal.aborted) setAccounts(previous => ({ ...previous, [id]: { snapshot } }))
          } catch (e) {
            if (signal.aborted) return
            const status = e instanceof ApiError ? e.status : null
            setAccounts(previous => ({
              ...previous,
              [id]: {
                snapshot: status === 401 || status === 403 ? undefined : previous[id]?.snapshot,
                error: status === 503 ? undefined : errorMessage(e),
                applying: status === 503,
              },
            }))
          }
        }))
      } catch (e) {
        if (signal.aborted) return
        setError(errorMessage(e))
        if (e instanceof ApiError && (e.status === 401 || e.status === 403)) {
          setSources(null)
          setAccounts({})
        }
      } finally {
        if (!signal.aborted) {
          setLoading(false)
          if (autoRefresh && !catalogOnly) timer = setTimeout(() => void load(), POLL_MS)
        }
      }
    }

    void load()
    return () => { controller.abort(); clearTimeout(timer) }
  }, [autoRefresh, catalogOnly, revision, selected])

  return { sources, accounts, error, loading, autoRefresh, setAutoRefresh, refresh: () => setRevision(v => v + 1) }
}

function replicationStatus({ snapshot, error, applying }: AccountData = {}): { label: string; tone: 'neutral' | 'warning' | 'success' | 'error'; detail?: string } {
  if (applying) return { label: '同步提交中', tone: 'warning' }
  if (error) return { label: '读取失败', tone: 'error', detail: error }
  if (!snapshot) return { label: '读取中', tone: 'neutral' }
  const replica = snapshot.replication
  if (!replica) return { label: '本地读取', tone: 'neutral' }
  if (replica.last_error) return { label: '同步失败', tone: 'error', detail: replica.last_error }
  if (replica.applying || !replica.initialized || replica.target_cursor !== null) return { label: '同步中', tone: 'warning' }
  if (snapshot.replication_stale) return { label: '同步延迟', tone: 'warning' }
  return { label: '已同步', tone: 'success' }
}

const runtimeLabels: Record<string, string> = {
  observing: '回报正常',
  waiting: '等待回报',
  recovering: '恢复中',
  disconnected: '已断线',
  transport_unavailable: '交易通道未就绪',
  blocked: '已阻断',
  stale: '回报延迟',
  stopped: '已停止',
}

export function executionStatus(snapshot?: api.Snapshot | null) {
  return snapshot ? runtimeLabels[snapshot.status] ?? snapshot.status : '—'
}

export function transportStatus(snapshot?: api.Snapshot | null) {
  return snapshot?.transport_ready == null ? '未报告' : snapshot.transport_ready ? '已连接' : '未连接'
}

export function tradeStatus(snapshot?: api.Snapshot | null) {
  return snapshot?.reported_can_trade == null ? '未报告' : snapshot.reported_can_trade ? '允许交易' : '暂停交易'
}

function SyncBadge({ data }: { data?: AccountData }) {
  const status = replicationStatus(data)
  return <span title={status.detail}><Badge tone={status.tone === 'error' ? 'warning' : status.tone}>{status.label}</Badge></span>
}

export function NativeWorkspace({ data }: { data: AccountsData }) {
  const { sources, accounts, error, loading, autoRefresh, setAutoRefresh, refresh } = data
  const [search, setSearch] = useState('')
  const [type, setType] = useState('')
  const [hidden, setHidden] = useState<Set<string>>(new Set())
  const rows = (sources ?? []).filter(row => !hidden.has(row.source_id)
    && (!type || type === row.adapter)
    && `${row.account} ${row.source_id}`.toLowerCase().includes(search.trim().toLowerCase()))
  const synced = rows.filter(row => replicationStatus(accounts[row.source_id]).tone === 'success').length
  const warnings = rows.filter(row => {
    const { tone } = replicationStatus(accounts[row.source_id])
    return tone === 'error' || tone === 'warning'
  }).length

  return <AppShell active="workspace" title="CTA 管理平台" subtitle="账户监控总览" icon={LayoutDashboard} wide className="overview-page">
    <section className="overview-toolbar" aria-label="总览筛选与统计">
      <div className="overview-filters">
        <label>账户类型
          <select value={type} onChange={event => setType(event.target.value)}>
            <option value="">全部类型</option>
            {[...new Map((sources ?? []).map(source => [source.adapter, accountType(source)]))].map(([id, name]) => <option key={id} value={id}>{name}</option>)}
          </select>
        </label>
        <label className="overview-auto"><input type="checkbox" checked={autoRefresh} onChange={event => setAutoRefresh(event.target.checked)} />自动刷新<span>{loading ? '更新中' : autoRefresh ? '每 15 秒' : '已暂停'}</span></label>
      </div>
      <div className="overview-stats" aria-live="polite">
        <OverviewStat label="账户数" value={sources ? String(rows.length) : '—'} />
        <OverviewStat label="已同步账户" value={sources ? String(synced) : '—'} />
        <OverviewStat label="同步告警" value={sources ? String(warnings) : '—'} warn={warnings > 0} />
      </div>
    </section>
    {error && <Alert tone="error" className="mb-4">{error}</Alert>}
    <AccountOverviewTable accounts={sources ?? []} search={search} setSearch={setSearch} hidden={hidden} setHidden={setHidden}
      refresh={refresh} refreshing={loading} empty={sources !== null && rows.length === 0}
      headings={<><th scope="col" className="overview-index">#</th>{['账号', '账户类型', '同步状态', '执行状态', '交易通道', '交易许可回报', '交易日', '委托', '成交', '源视图时间', '最近同步提交'].map(label => <th key={label} scope="col">{label}</th>)}</>}>
      {rows.map((source, index) => {
        const account = accounts[source.source_id]
        const snapshot = account?.snapshot
        return <tr key={source.source_id} data-source={source.source_id}>
          <td className="overview-index">{index + 1}</td>
          <th scope="row" className="overview-account-cell"><a href={routes.account(source.source_id)}>{source.account}</a></th>
          <td>{accountType(source)}</td>
          <td><SyncBadge data={account} /></td>
          <td>{executionStatus(snapshot)}</td>
          <td>{transportStatus(snapshot)}</td>
          <td>{tradeStatus(snapshot)}</td>
          <td>{snapshot?.trading_day ?? '—'}</td>
          <td>{snapshot?.total_orders ?? '—'}</td>
          <td>{snapshot?.total_fills ?? '—'}</td>
          <td className="overview-time">{time(snapshot?.replication?.source_captured_at_us)}</td>
          <td className="overview-time">{time(snapshot?.replication?.last_synced_at_us)}</td>
        </tr>
      })}
    </AccountOverviewTable>
  </AppShell>
}

export function NativeAccounts({ data }: { data: AccountsData }) {
  const { sources, accounts, error, loading, refresh } = data
  const synced = sources?.filter(source => replicationStatus(accounts[source.source_id]).tone === 'success').length
  return <AppShell active="accounts" title="CTA 管理平台" subtitle="账户管理" icon={WalletCards}>
    <PageIntro eyebrow="账户管理" title="账户入口"
      actions={<Button type="button" onClick={refresh} disabled={loading} aria-label="刷新数据"><RefreshCw size={15} /></Button>} />
    {error && <Alert tone="error" className="mb-4">{error}</Alert>}
    <div className="mb-8 grid gap-3 sm:grid-cols-2">
      <StatTile label="已同步账户" value={synced == null ? '—' : String(synced)} />
      <StatTile label="已授权账户" value={sources ? String(sources.length) : '—'} />
    </div>
    <div className="grid gap-4 md:grid-cols-2">
      {sources?.map(source => {
        const account = accounts[source.source_id]
        const snapshot = account?.snapshot
        const positions = snapshot?.snapshot_ready
          ? new Set(snapshot.positions.filter(p => p.position.today + p.position.yesterday > 0).map(p => `${p.venue}:${p.symbol}`)).size
          : null
        return <AccountCardLayout key={source.source_id} title={source.account} mark={source.adapter === 'yida' ? 'YD' : 'CTP'}
          sourceId={source.source_id} badges={<><Badge tone="brand">{accountType(source)}</Badge><SyncBadge data={account} /></>}>
          <div className="grid grid-cols-2 gap-3 text-sm">
            <AccountMetric label="执行状态" value={executionStatus(snapshot)} />
            <AccountMetric label="交易通道" value={transportStatus(snapshot)} />
            <AccountMetric label="交易许可回报" value={tradeStatus(snapshot)} />
            <AccountMetric label="持仓快照" value={positions === null ? '—' : `${positions} 个合约`} />
            <AccountMetric label="最近同步提交" value={time(snapshot?.replication?.last_synced_at_us)} />
          </div>
          <div className="flex flex-wrap gap-2 border-t border-border-soft pt-4">
            <AccountActionLink href={routes.account(source.source_id)} primary icon={<WalletCards size={15} />} label="账户详情" />
            <AccountActionLink href={routes.nav(source.source_id)} icon={<Activity size={15} />} label="净值" />
            <AccountActionLink href={routes.executionCost(source.source_id)} icon={<Scale size={15} />} label="成本" />
          </div>
        </AccountCardLayout>
      })}
    </div>
    {sources?.length === 0 && <p className="py-8 text-center text-sm text-muted">暂无授权账户</p>}
  </AppShell>
}
