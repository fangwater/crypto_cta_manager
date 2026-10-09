import { useCallback, useEffect, useState } from 'react'
import { deleteVirtualAccount, listVirtualAccounts, saveVirtualAccount } from '../api'
import { useAuth } from '../components/AuthGate'
import { Layers3, Plus, RefreshCw, Users } from 'lucide-react'
import { AppShell, PageIntro, StatTile } from '../components/AppShell'
import { routes } from '../lib/routes'
import { timestampUs } from '../format'
import { cn } from '../lib/cn'
import { Card, CardContent, CardHeader, CardTitle } from '../components/ui/Card'
import { Button } from '../components/ui/Button'
import { FieldHint, Input, Label, Select } from '../components/ui/Field'
import { Alert, Badge } from '../components/ui/Badge'
import { useConfigWrite } from '../hooks/useConfigWrite'
import { useStrategyCatalog } from '../hooks/useStrategyCatalog'
import type { VirtualAccount, VirtualBinding } from '../types'

export function VirtualAccountsPage() {
  const { user } = useAuth()
  const isAdmin = user.role === 'admin'
  const { positions, orders, loading, error: catalogError } = useStrategyCatalog()
  const { withWrite, saving, error: writeError, notice } = useConfigWrite()
  const [accounts, setAccounts] = useState<VirtualAccount[]>([])
  const [selected, setSelected] = useState('')
  const [virtualId, setVirtualId] = useState('')
  const [name, setName] = useState('')
  const [bindings, setBindings] = useState<VirtualBinding[]>([])
  const [experimentalToken, setExperimentalToken] = useState('')
  const [readError, setReadError] = useState<string | null>(null)
  const [reading, setReading] = useState(true)
  const [shareInputs, setShareInputs] = useState<Record<string, string>>({})
  const [query, setQuery] = useState('')
  const reload = useCallback(async (signal?: AbortSignal) => {
    const next = await listVirtualAccounts(signal)
    setAccounts(next)
    setReadError(null)
  }, [])
  useEffect(() => {
    const controller = new AbortController()
    const load = () => reload(controller.signal).catch((reason: unknown) => {
      if (reason instanceof DOMException && reason.name === 'AbortError') return
      setReadError(reason instanceof Error ? reason.message : String(reason))
    }).finally(() => setReading(false))
    void load()
    const timer = window.setInterval(() => void load(), 10000)
    return () => { controller.abort(); window.clearInterval(timer) }
  }, [reload])
  function select(id: string) {
    const account = accounts.find((a) => a.virtual_id === id)
    setSelected(id)
    setVirtualId(id)
    setName(account?.name ?? '')
    setBindings(account?.bindings.map((b) => ({ ...b })) ?? [])
    setShareInputs(Object.fromEntries((account?.bindings ?? []).map((b) => [b.binding_name, String(b.shares)])))
  }
  const available = positions.filter((p) => !bindings.some((b) => b.position_strategy_name === p.strategy_name))
  const validShares = bindings.every((b) => {
    const value = shareInputs[b.binding_name] ?? String(b.shares)
    return value.trim() !== '' && Number.isFinite(Number(value)) && Number(value) >= 0
  })
  const selectedAccount = accounts.find((account) => account.virtual_id === selected)
  const filtered = accounts.filter((account) => `${account.name} ${account.virtual_id}`.toLowerCase().includes(query.toLowerCase()))
  const followerCount = new Set(accounts.flatMap((account) => account.followers.map((follower) => follower.source_id))).size
  return <AppShell active="virtual" title="Virtual 账户" subtitle="策略组合管理工作台" icon={Layers3}>
    <PageIntro eyebrow="Virtual Accounts" title="Virtual 账户管理" description="独立维护策略组合，实际账户按倍率跟随。" actions={<div className="flex gap-2">
      <Button type="button" disabled={saving || reading} onClick={() => void withWrite(async () => { await reload(); return '列表与同步状态已刷新。' })}><RefreshCw size={15} /> 刷新</Button>
      {isAdmin && <Button type="button" variant="primary" disabled={saving || reading} onClick={() => select('')}><Plus size={15} /> 新建 Virtual</Button>}
    </div>} />
    {(readError ?? catalogError ?? writeError) && <Alert className="mb-4" tone="error">{readError ?? catalogError ?? writeError}</Alert>}
    {notice && <Alert className="mb-4" tone="success">{notice}</Alert>}
    {!isAdmin && <p className="mb-4 text-sm text-muted">当前为只读视图。Virtual 账户由管理员维护。</p>}
    <div className="mb-6 grid gap-3 sm:grid-cols-3">
      <StatTile label="Virtual 账户" value={reading ? '--' : String(accounts.length)} />
      <StatTile label="策略绑定" value={reading ? '--' : String(accounts.reduce((sum, account) => sum + account.bindings.length, 0))} />
      <StatTile label="跟随账户" value={reading ? '--' : String(followerCount)} />
    </div>
    <div className="grid items-start gap-6 lg:grid-cols-[280px_minmax(0,1fr)]">
      <Card><CardHeader><CardTitle>账户列表</CardTitle></CardHeader><CardContent className="space-y-3">
        <Input aria-label="搜索 Virtual 账户" placeholder="搜索名称或 ID" value={query} onChange={(event) => setQuery(event.target.value)} />
        {reading ? <p className="text-sm text-muted">正在加载…</p> : filtered.length === 0 ? <p className="text-sm text-muted">{accounts.length ? '没有匹配的账户。' : '暂无 Virtual 账户。'}</p> : filtered.map((account) => <button type="button" key={account.virtual_id} disabled={saving} onClick={() => select(account.virtual_id)} aria-pressed={selected === account.virtual_id}
          className={cn('w-full rounded-xl border p-3 text-left transition-colors', selected === account.virtual_id ? 'border-brand-ring bg-brand-soft' : 'border-border hover:bg-canvas')}>
          <span className="block truncate font-medium text-ink">{account.name}</span>
          <span className="mt-1 block truncate text-xs text-muted">{account.virtual_id}</span>
          <span className="mt-2 block text-xs text-muted">{account.bindings.length} 条策略 · {account.followers.length} 个跟随账户</span>
        </button>)}
      </CardContent></Card>
      <div className="min-w-0 space-y-4">
      <Card><CardHeader><CardTitle>{selected ? name : isAdmin ? '新建 Virtual 账户' : '选择 Virtual 账户查看组合'}</CardTitle></CardHeader><CardContent className="space-y-4">
      <form className="space-y-5" onSubmit={(event) => {
        event.preventDefault()
        if (!isAdmin || !validShares) return
        void withWrite(async () => {
          const saved = await saveVirtualAccount(virtualId.trim(), name.trim(), bindings.map((b) => ({ ...b, shares: Number(shareInputs[b.binding_name] ?? b.shares) })), experimentalToken)
          setAccounts(await listVirtualAccounts())
          setSelected(saved.virtual_id)
          setVirtualId(saved.virtual_id)
          setBindings(saved.bindings)
          setShareInputs(Object.fromEntries(saved.bindings.map((b) => [b.binding_name, String(b.shares)])))
          setExperimentalToken('')
          return 'Virtual 配置已保存；跟随账户自动同步，发布状态可在账户的策略启用页查看。'
        })
      }}>
        <fieldset disabled={!isAdmin || saving || loading || reading} className="space-y-5">
          <div className="grid gap-4 sm:grid-cols-2">
            <Label>稳定 ID<Input value={virtualId} disabled={!!selected} onChange={(event) => setVirtualId(event.target.value)} /><FieldHint>创建后保持不变。</FieldHint></Label>
            <Label>名称<Input value={name} onChange={(event) => setName(event.target.value)} /></Label>
          </div>
          <div className="space-y-4">
            {bindings.length === 0 && <p className="text-sm text-muted">暂未配置策略。保存空组合会停止跟随账户中原有的策略。</p>}
            {bindings.map((b, index) => <div key={b.binding_name} className="grid items-end gap-3 rounded-xl border border-border p-3 md:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_7rem_auto]">
              <Label>仓位策略<Input value={b.position_strategy_name} readOnly /><FieldHint>发布名：{b.binding_name}</FieldHint></Label>
              <Label>下单模板<Select value={b.order_strategy_name} onChange={(event) => setBindings((old) => old.map((item, i) => i === index ? { ...item, order_strategy_name: event.target.value } : item))}>
                {orders.map((o) => <option key={o.strategy_name} value={o.strategy_name}>{o.strategy_name} ({o.order_parameters.algorithm.toUpperCase()})</option>)}
              </Select></Label>
              <Label>Virtual 份数<Input inputMode="decimal" value={shareInputs[b.binding_name] ?? String(b.shares)} onChange={(event) => setShareInputs((old) => ({ ...old, [b.binding_name]: event.target.value }))} /></Label>
              <Button type="button" variant="ghost" onClick={() => setBindings((old) => old.filter((_, i) => i !== index))}>移除</Button>
            </div>)}
            <Label>添加仓位策略<Select value="" onChange={(event) => {
              if (!event.target.value || !orders[0]) return
              const strategy = event.target.value
              setBindings((old) => [...old, { binding_name: strategy, position_strategy_name: strategy, order_strategy_name: orders[0].strategy_name, shares: 1 }])
              setShareInputs((old) => ({ ...old, [strategy]: '1' }))
            }} disabled={!orders.length || !available.length}>
              <option value="">选择策略以添加</option>
              {available.map((p) => <option key={p.strategy_name} value={p.strategy_name}>{p.strategy_name}</option>)}
            </Select></Label>
          </div>
          <Label>实验算法 Token<Input type="password" autoComplete="off" value={experimentalToken} onChange={(event) => setExperimentalToken(event.target.value)} /><FieldHint>修改涉及跟随账户首次启用或重新启用 POV/Chase 时使用。</FieldHint></Label>
          <FieldHint>实际账户的生效份数 = 这里的份数 × 跟随倍率。保存后自动更新全部跟随账户；移除策略会发布原策略名的零目标。</FieldHint>
          <div className="flex flex-wrap gap-3">
            <Button type="submit" variant="primary" disabled={!virtualId.trim() || !name.trim() || !validShares}>保存组合并同步</Button>
            {selected && <Button type="button" variant="ghost" onClick={() => void withWrite(async () => {
              await deleteVirtualAccount(selected)
              setAccounts(await listVirtualAccounts())
              select('')
              return 'Virtual 账户已删除。'
            })}>删除 Virtual</Button>}
          </div>
        </fieldset>
      </form>
    </CardContent></Card>
    {selectedAccount && <Card><CardHeader><CardTitle className="flex items-center gap-2"><Users size={16} /> 跟随账户</CardTitle></CardHeader><CardContent className="space-y-3">
      <p className="text-xs text-muted">最近更新：{timestampUs(selectedAccount.updated_at_us)} · 同步状态每 10 秒刷新</p>
      {selectedAccount.followers.length === 0 ? <p className="text-sm text-muted">尚无可查看的跟随账户。在实际账户的策略启用页选择 Follow virtual 和倍率。</p> : selectedAccount.followers.map((follower) => <div key={follower.source_id} className="rounded-xl border border-border p-3">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <a href={routes.configBindings(follower.source_id)} className="break-all text-sm font-medium text-brand">{follower.source_id} →</a>
          <Badge tone={follower.pending_publishes.length ? 'warning' : 'success'}>{follower.pending_publishes.length ? `待同步 ${follower.pending_publishes.length} 条` : '已同步'}</Badge>
        </div>
        <p className="mt-2 text-sm text-muted">跟随倍率：{follower.multiplier}x</p>
        {follower.pending_publishes.map((pending) => <p key={pending.binding_name} className="mt-1 break-words text-xs text-muted">{pending.binding_name}：{pending.error ?? '等待发布'}</p>)}
      </div>)}
    </CardContent></Card>}
    </div></div>
  </AppShell>
}
