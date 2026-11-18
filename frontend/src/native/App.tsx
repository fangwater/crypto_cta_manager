import { lazy, Suspense, useCallback, useEffect, useState, type ReactNode } from 'react'
import { Activity, Plus, RefreshCw, Scale, Settings, Shield, Trash2, WalletCards } from 'lucide-react'
import { useAuth } from '../components/AuthGate'
import { AppShell, PageIntro, StatTile } from '../components/AppShell'
import { Tabs } from '../components/ui/Tabs'
import { normalizePath, readSourceId, routes } from '../lib/routes'
import {
  accountType, errorMessage, executionStatus, money, NativeAccounts, NativeWorkspace,
  time, tradeStatus, transportStatus, useNativeAccounts,
} from './Accounts'
import { Button } from '../components/ui/Button'
import { Alert } from '../components/ui/Badge'
import { Card, CardContent } from '../components/ui/Card'
import { Input, Label, Select } from '../components/ui/Field'
import { createAuthUser, listAuthUsers, type AuthUser } from '../api'
import * as api from './api'
import CostAnalysis from './CostAnalysis'
import './native.css'

const NavAnalysis = lazy(() => import('./NavAnalysis'))

const statuses: Record<string, string> = { submitted_locally: '本地已提交', rejected_locally: '本地拒绝', observed: '已见回报', prepared: '发送前', pending: '等待回报', working: '已受理', partially_filled: '部分成交', cancel_pending: '撤单待确认', settling: '等待成交事实', filled: '已成交', cancelled: '已撤单', rejected: '已拒绝', uncertain: '发送结果不确定' }
const offsets: Record<string, string> = { open: '开仓', close: '平仓', close_today: '平今', close_yesterday: '平昨' }
const venues = ['SHFE', 'INE', 'DCE', 'CZCE', 'CFFEX', 'GFEX']
const label = (value: string) => statuses[value] ?? value
const side = (value: string) => value === 'buy' ? '买' : '卖'
const accountTabs = [
  { id: 'account', label: '持仓' },
  { id: 'orders', label: '委托' },
  { id: 'fills', label: '成交' },
] as const
const navTab = { id: 'nav', label: '净值' } as const
type AccountTab = typeof accountTabs[number]['id'] | typeof navTab.id

function Table({ headers, children }: { headers: string[]; children: ReactNode }) {
  return <div className="native-table-scroll"><table className="native-table"><thead><tr>{headers.map(h => <th key={h}>{h}</th>)}</tr></thead><tbody>{children}</tbody></table></div>
}

export default function NativeApp() {
  const path = normalizePath(location.pathname)
  const workspace = path === normalizePath(routes.workspace)
  const accounts = path === normalizePath(routes.accounts)
  const strategies = path === normalizePath(routes.configPosition) || path === '/manager/config'
  const permissions = path === normalizePath(routes.admin)
  const [requestedSource, setRequestedSource] = useState(readSourceId)
  const data = useNativeAccounts(workspace || accounts ? undefined : requestedSource, strategies || permissions)

  if (workspace) return <NativeWorkspace data={data} />
  if (accounts) return <NativeAccounts data={data} />

  const source = data.sources?.find(row => row.source_id === requestedSource) ?? data.sources?.[0]
  const chooseSource = (id: string) => {
    const url = new URL(location.href)
    url.searchParams.set('source', id)
    history.replaceState(null, '', url)
    setRequestedSource(id)
  }
  const selectSource = <Select className="w-auto max-w-full" aria-label="账户" value={source?.source_id ?? ''} onChange={event => chooseSource(event.target.value)}>
    {!data.sources?.length && <option value="">{data.loading ? '加载账户…' : '暂无授权账户'}</option>}
    {data.sources?.map(row => <option key={row.source_id} value={row.source_id}>{row.account}</option>)}
  </Select>

  if (strategies) return <AppShell active="config" title="CTA 管理平台" subtitle="策略配置" icon={Settings}>
    <PageIntro eyebrow="策略" title="仓位策略" />
    <Strategies />
  </AppShell>

  if (permissions) return <AppShell active="admin" title="CTA 管理平台" subtitle="访问权限" icon={Shield}>
    <PageIntro eyebrow="权限" title="账户与策略授权" actions={selectSource} />
    {data.error && <Alert tone="error" className="mb-4">{data.error}</Alert>}
    <Permissions source={source} />
  </AppShell>

  const account = path === '/manager/account'
  const navMonitor = path === normalizePath(routes.navMonitor)
  const cost = path === normalizePath(routes.executionCost())
  const active = account ? 'accounts' : navMonitor ? 'nav-monitor' : cost ? 'acquisition-cost' : 'manager'
  const title = account ? '账户详情' : navMonitor ? '账户净值' : cost ? '执行成本' : '净值分析'
  const Icon = account ? WalletCards : cost ? Scale : Activity
  const sourceId = source?.source_id ?? ''
  const result = data.accounts[sourceId]
  const snapshot = result?.snapshot ?? null
  const error = data.error ?? result?.error
  const nav = <Suspense fallback={<p className="text-sm text-muted">正在加载净值页面…</p>}>
    <NavAnalysis key={sourceId} sourceId={sourceId} snapshot={snapshot} canConfigure={source?.access_level === 'configure'} />
  </Suspense>

  return <AppShell active={active} title="CTA 管理平台" subtitle={title} icon={Icon} wide>
    <PageIntro eyebrow={title} title={source?.account ?? title} description={source && accountType(source)} />
    <div className="mb-5 flex flex-wrap items-center gap-3">
      {selectSource}
      <label className="flex items-center gap-2 text-sm text-muted"><input type="checkbox" checked={data.autoRefresh} onChange={event => data.setAutoRefresh(event.target.checked)} />自动刷新</label>
      <Button onClick={data.refresh} disabled={data.loading} aria-label="刷新数据"><RefreshCw size={14} />刷新</Button>
      {snapshot && <span className="text-sm text-muted">{executionStatus(snapshot)} · 交易日 {snapshot.trading_day ?? '—'} · 查询 {time(snapshot.read_at_us)}</span>}
    </div>
    {error && <Alert tone="error" className="mb-4">{error}{snapshot ? `；当前展示 ${time(snapshot.read_at_us)} 读取的数据` : ''}</Alert>}
    {result?.applying && <Alert tone="warning" className="mb-4">同步副本正在提交，稍后可重新读取。</Alert>}
    {snapshot?.replication && <div className="mb-4 space-y-2">
      <p className="text-sm text-muted">本地同步副本 · 最近提交 {time(snapshot.replication.last_synced_at_us)} · 源视图 {time(snapshot.replication.source_captured_at_us)}{snapshot.replication.target_cursor !== null ? ` · 待提交 ${snapshot.replication.target_cursor - snapshot.replication.committed_cursor} 条修订` : ''}</p>
      {(snapshot.replication.last_error || snapshot.replication_stale) && <Alert tone="warning">{snapshot.replication.last_error ?? '同步数据已超过配置的新鲜度限制。'} 以下展示已提交的本地历史数据。</Alert>}
    </div>}
    {!source ? <EmptyAccount loading={data.loading} />
      : cost ? <CostAnalysis key={sourceId} sourceId={sourceId} tradingDay={snapshot?.trading_day} />
      : navMonitor ? nav
      : <AccountDetail key={sourceId} snapshot={snapshot} loading={data.loading} nav={account ? undefined : nav} />}
  </AppShell>
}

function EmptyAccount({ loading }: { loading: boolean }) {
  return <Card><CardContent className="py-12 text-center text-muted">{loading ? '正在读取账户记录…' : '暂无账户数据'}</CardContent></Card>
}

function AccountDetail({ snapshot, loading, nav }: { snapshot: api.Snapshot | null; loading: boolean; nav?: ReactNode }) {
  const [tab, setTab] = useState<AccountTab>(nav ? 'nav' : 'account')
  const tabs = nav ? [navTab, ...accountTabs] : [...accountTabs]
  return <>
    <Tabs<AccountTab> value={tab} onChange={setTab} items={tabs} className="mb-4" />
    {tab === 'nav' ? nav : !snapshot ? <EmptyAccount loading={loading} /> : <>
        {snapshot.reason && <Alert tone="warning" className="mb-4">{snapshot.reason}</Alert>}
        {snapshot.positions_may_lag && <Alert tone="warning" className="mb-4">已有较新的事实成交，持仓仍显示 {time(snapshot.state_observed_us)} 的快照。</Alert>}
        {tab === 'account' && <>
          <div className="mb-6 grid grid-cols-2 gap-3 lg:grid-cols-4">
            <StatTile label="本地可用预算（元）" value={money(snapshot.available_local_budget)} />
            <StatTile label="委托记录" value={String(snapshot.total_orders)} />
            <StatTile label="事实成交" value={String(snapshot.total_fills)} />
            <StatTile label="投影错误" value={String(snapshot.projection_errors)} />
          </div>
          <p className="mb-3 text-sm text-muted">持仓快照 {time(snapshot.state_observed_us)} · 快照恢复 {snapshot.snapshot_ready ? snapshot.snapshot_current_runtime ? '已追平' : '已追平（旧进程）' : '未就绪'} · 通道 {transportStatus(snapshot)} · 交易许可回报 {tradeStatus(snapshot)}</p>
          <Table headers={['交易所', '合约', '方向', '今仓（手）', '昨仓（手）', '冻结今仓', '冻结昨仓']}>
            {snapshot.positions.map(p => <tr key={`${p.venue}:${p.symbol}:${p.side}`}><td>{p.venue}</td><td>{p.symbol}</td><td>{p.side === 'buy' ? '多' : '空'}</td><td>{p.position.today}</td><td>{p.position.yesterday}</td><td>{p.position.frozen_today}</td><td>{p.position.frozen_yesterday}</td></tr>)}
          </Table>
        </>}
        {tab === 'orders' && <><p className="mb-3 text-sm text-muted">最近 {snapshot.orders.length} 条 · 全部 {snapshot.total_orders} 条</p><Table headers={['更新时间', '交易日', '交易所／合约', '会话／引用', '方向／开平', '类型', '手数', '事实／累计成交', '观察状态', '执行端状态', '发送状态', '系统号']}>
          {snapshot.orders.map(({ record: r, phase }) => <tr key={`${r.identity.trading_day}:${r.identity.venue}:${r.identity.native.session}:${r.identity.native.order_ref}`}><td>{time(r.updated_us)}</td><td>{r.identity.trading_day}</td><td>{r.identity.venue}／{r.order.request.symbol}</td><td>{r.identity.native.session}／{r.identity.native.order_ref}</td><td>{side(r.order.request.side)}／{offsets[r.order.request.offset]}</td><td>{r.order.request.price_type === 'market' ? '市价' : `限价 ${r.order.limit_price}`} {r.order.request.time_in_force.toUpperCase()}</td><td>{r.order.request.lots}</td><td>{r.order.filled}／{r.order.reported_filled}</td><td>{label(phase)}</td><td>{label(r.order.status)}</td><td>{label(r.send_state)}</td><td>{r.order.system_id || '—'}</td></tr>)}
        </Table></>}
        {tab === 'fills' && <><p className="mb-3 text-sm text-muted">最近 {snapshot.fills.length} 条 · 全部 {snapshot.total_fills} 条。手续费按合约及开仓、平仓、平今、平昨规则计费；柜台未报告的实际扣费显示为“未报告”。</p><Table headers={['接收时间', '交易日', '交易所／合约', '成交号', '方向／开平', '手数', '成交价', '事实手续费', '柜台原生时间']}>
          {snapshot.fills.map(r => <tr key={`${r.fill.identity.trading_day}:${r.fill.identity.venue}:${r.fill.identity.symbol}:${r.fill.identity.trade_id}:${r.fill.identity.side}`}><td>{time(r.received_us)}</td><td>{r.fill.identity.trading_day}</td><td>{r.fill.identity.venue}／{r.fill.identity.symbol}</td><td>{r.fill.identity.trade_id}</td><td>{side(r.fill.identity.side)}／{offsets[r.fill.offset]}</td><td>{r.fill.lots}</td><td>{r.fill.price}</td><td>{r.commission === null ? '未报告' : `${money(r.commission)} ${r.commission_currency ?? ''}`}</td><td>{r.native_time ?? '—'}</td></tr>)}
        </Table></>}
    </>}
  </>
}

type TargetRow = { venue: string; symbol: string; qty: string; signal: number }
function Strategies() {
  const [items, setItems] = useState<api.Strategy[]>([])
  const [name, setName] = useState('')
  const [rows, setRows] = useState<TargetRow[]>([])
  const [error, setError] = useState<string | null>(null)
  const [notice, setNotice] = useState('')
  const [saving, setSaving] = useState(false)
  const reload = useCallback(() => api.getStrategies().then(setItems), [])
  useEffect(() => { void reload().catch(e => setError(errorMessage(e))) }, [reload])
  const selected = items.find(i => i.strategy.strategy_name === name)
  const editable = !selected || selected.can_configure
  const choose = (name: string) => {
    setName(name); setNotice('')
    const strategy = items.find(i => i.strategy.strategy_name === name)?.strategy
    setRows(Object.entries(strategy?.targets ?? {}).map(([market, target]) => { const [venue, symbol] = market.split(':'); return { venue, symbol, qty: String(target.qty === 'clear' ? 0 : target.qty), signal: target.signal } }))
  }
  return <div className="space-y-4">
    {error && <Alert tone="error">{error}</Alert>}{notice && <p role="status" className="text-sm text-brand">{notice}</p>}
    <div className="flex flex-wrap items-center gap-3"><Select aria-label="策略" value={selected ? name : ''} onChange={e => choose(e.target.value)}><option value="">新建策略</option>{items.map(i => <option key={i.strategy.strategy_name}>{i.strategy.strategy_name}</option>)}</Select><Button onClick={() => choose('')}><Plus size={14} />新建</Button></div>
    <Card><CardContent className="space-y-4 py-5"><Label>策略名称<Input value={name} onChange={e => setName(e.target.value)} disabled={!!selected} /></Label>
      <p className="text-sm text-muted">目标单位为手，正数为多仓，负数为空仓。保存策略仅更新管理配置。</p>
      {rows.map((row, index) => <div className="native-target-row" key={index}><Select aria-label="交易所" value={row.venue} disabled={!editable} onChange={e => setRows(old => old.map((r, j) => j === index ? { ...r, venue: e.target.value } : r))}>{venues.map(venue => <option key={venue}>{venue}</option>)}</Select><Input aria-label="合约" value={row.symbol} disabled={!editable} onChange={e => setRows(old => old.map((r, j) => j === index ? { ...r, symbol: e.target.value } : r))} /><Input aria-label="目标手数" type="number" step="1" value={row.qty} disabled={!editable} onChange={e => setRows(old => old.map((r, j) => j === index ? { ...r, qty: e.target.value } : r))} /><Button aria-label="移除目标" disabled={!editable} onClick={() => setRows(old => old.filter((_, j) => j !== index))}><Trash2 size={14} /></Button></div>)}
      <div className="flex gap-3"><Button disabled={!editable} onClick={() => setRows(old => [...old, { venue: 'SHFE', symbol: '', qty: '0', signal: 0 }])}><Plus size={14} />添加合约</Button><Button variant="primary" disabled={saving || !editable || !name.trim()} onClick={() => {
        setSaving(true); setError(null); setNotice('')
        const targets: api.Strategy['strategy']['targets'] = {}
        for (const row of rows) { const key = `${row.venue}:${row.symbol.trim()}`; if (!row.qty.trim() || !Number.isSafeInteger(Number(row.qty)) || key in targets) { setError('手数必须为整数，合约不能重复'); setSaving(false); return } targets[key] = { qty: Number(row.qty), signal: row.signal } }
        void api.saveStrategy(name.trim(), targets).then(reload).then(() => setNotice('策略已保存')).catch(e => setError(errorMessage(e))).finally(() => setSaving(false))
      }}>保存策略</Button></div>
    </CardContent></Card>
  </div>
}

function Permissions({ source }: { source?: api.Source }) {
  const { user } = useAuth()
  const [users, setUsers] = useState<AuthUser[]>([])
  const [grants, setGrants] = useState<api.Grant[]>([])
  const [strategies, setStrategies] = useState<api.Strategy[]>([])
  const [strategy, setStrategy] = useState('')
  const [strategyAccess, setStrategyAccess] = useState<api.StrategyGrants | null>(null)
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  useEffect(() => { void Promise.all([listAuthUsers().then(setUsers), api.getStrategies().then(rows => setStrategies(rows.filter(r => r.can_configure)))]).catch(e => setError(errorMessage(e))) }, [])
  useEffect(() => { setGrants([]); if (source?.access_level === 'configure') { let active = true; void api.getGrants(source.source_id).then(rows => { if (active) setGrants(rows) }).catch(e => { if (active) setError(errorMessage(e)) }); return () => { active = false } } }, [source])
  useEffect(() => { setStrategyAccess(null); if (!strategy) return; let active = true; void api.getStrategyGrants(strategy).then(v => { if (active) setStrategyAccess(v) }).catch(e => { if (active) setError(errorMessage(e)) }); return () => { active = false } }, [strategy])
  const implicit = (kind: 'account' | 'strategy', u: AuthUser) => kind === 'strategy' && (u.role === 'admin' || strategyAccess?.created_by === u.username || strategyAccess?.managers.some(m => m.user_id === u.user_id) === true)
  const grantRows = (kind: 'account' | 'strategy', values: api.Grant[]) => <Table headers={['用户', kind === 'account' ? '显式授权' : '权限']}>{users.map(u => <tr key={u.user_id}><td>{u.username}{u.role === 'admin' ? '（管理员）' : ''}</td><td><Select aria-label={`${u.username}权限`} disabled={busy || implicit(kind, u)} value={implicit(kind, u) ? 'configure' : values.find(g => g.user_id === u.user_id)?.access_level ?? 'none'} onChange={e => {
    const next = values.filter(g => g.user_id !== u.user_id); if (e.target.value !== 'none') next.push({ user_id: u.user_id, access_level: e.target.value })
    setBusy(true); setError(null)
    const job = kind === 'account' ? api.saveGrants(source!.source_id, next).then(setGrants) : api.saveStrategyGrants(strategy, next, strategyAccess?.open_visibility ?? false).then(setStrategyAccess)
    void job.catch(e => setError(errorMessage(e))).finally(() => setBusy(false))
  }}><option value="none">未单独授权</option><option value="view">查看</option><option value="configure">{implicit(kind, u) ? '配置（隐式权限）' : '配置'}</option></Select></td></tr>)}</Table>
  return <div className="space-y-5">{error && <Alert tone="error">{error}</Alert>}
    {source?.access_level === 'configure' && <section><h2 className="mb-3 font-semibold">{source.account} · 账户授权</h2>{grantRows('account', grants)}</section>}
    <section><h2 className="mb-3 font-semibold">策略授权</h2><Select aria-label="授权策略" value={strategy} onChange={e => setStrategy(e.target.value)}><option value="">选择策略</option>{strategies.map(s => <option key={s.strategy.strategy_name}>{s.strategy.strategy_name}</option>)}</Select>{strategyAccess && <><p className="my-3 text-sm text-muted">创建者 {strategyAccess.created_by ?? '—'} · 创建者与管理员拥有配置权限</p><label className="mb-3 flex gap-2 text-sm"><input type="checkbox" checked={strategyAccess.open_visibility} disabled={busy} onChange={e => { setBusy(true); void api.saveStrategyGrants(strategy, strategyAccess.grants, e.target.checked).then(setStrategyAccess).catch(e => setError(errorMessage(e))).finally(() => setBusy(false)) }} />所有登录用户可查看</label>{grantRows('strategy', strategyAccess.grants)}</>}</section>
    {user.role === 'admin' && <Card><CardContent className="space-y-3 py-5"><h2 className="font-semibold">创建用户</h2><Label>用户名<Input value={username} onChange={e => setUsername(e.target.value)} autoComplete="off" /></Label><Label>密码<Input type="password" value={password} onChange={e => setPassword(e.target.value)} autoComplete="new-password" /></Label><Button disabled={busy || !username || password.length < 8} onClick={() => { setBusy(true); void createAuthUser(username, password).then(u => { setUsers(old => [...old, u]); setUsername(''); setPassword('') }).catch(e => setError(errorMessage(e))).finally(() => setBusy(false)) }}>创建用户</Button></CardContent></Card>}
  </div>
}
