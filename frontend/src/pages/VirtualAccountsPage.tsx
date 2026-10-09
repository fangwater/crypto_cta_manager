import { useCallback, useEffect, useRef, useState } from 'react'
import { Layers3, Plus, RefreshCw, Save, Trash2, Users } from 'lucide-react'
import { createVirtualAccount, deleteVirtualAccount, listVirtualAccounts, listVirtualGrantees, saveVirtualAccount, saveVirtualGrants } from '../api'
import { useAuth } from '../components/AuthGate'
import { AppShell, PageIntro } from '../components/AppShell'
import { routes } from '../lib/routes'
import { timestampUs } from '../format'
import { cn } from '../lib/cn'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '../components/ui/Card'
import { Button } from '../components/ui/Button'
import { FieldHint, Input, Label, Select } from '../components/ui/Field'
import { Alert, Badge } from '../components/ui/Badge'
import { useConfigWrite } from '../hooks/useConfigWrite'
import { useStrategyCatalog } from '../hooks/useStrategyCatalog'
import type { VirtualAccount, VirtualBinding } from '../types'

export function VirtualAccountsPage() {
  const { user } = useAuth()
  const { positions, orders, loading, error: catalogError } = useStrategyCatalog()
  const { withWrite, saving, error: writeError, notice, setError, setNotice } = useConfigWrite()
  const [accounts, setAccounts] = useState<VirtualAccount[]>([])
  const [selected, setSelected] = useState('')
  const [name, setName] = useState('')
  const [bindings, setBindings] = useState<VirtualBinding[]>([])
  const [readError, setReadError] = useState<string | null>(null)
  const [reading, setReading] = useState(true)
  const [shareInputs, setShareInputs] = useState<Record<string, string>>({})
  const [query, setQuery] = useState('')
  const [strategyToAdd, setStrategyToAdd] = useState('')
  const [grantees, setGrantees] = useState<{ user_id: number; username: string }[]>([])
  const [grantUserIds, setGrantUserIds] = useState<number[]>([])
  const initialized = useRef(false)

  const openAccount = useCallback((account?: VirtualAccount) => {
    setSelected(account?.virtual_id ?? '')
    setName(account?.name ?? '')
    setBindings(account?.bindings.map((binding) => ({ ...binding })) ?? [])
    setShareInputs(Object.fromEntries((account?.bindings ?? []).map((binding) => [binding.binding_name, String(binding.shares)])))
    setStrategyToAdd('')
  }, [])
  const reload = useCallback(async (signal?: AbortSignal) => {
    const next = await listVirtualAccounts(signal)
    setAccounts(next)
    setReadError(null)
    if (!initialized.current) {
      initialized.current = true
      openAccount(next[0])
    }
  }, [openAccount])
  useEffect(() => {
    const controller = new AbortController()
    const load = () => reload(controller.signal).catch((reason: unknown) => {
      if (reason instanceof DOMException && reason.name === 'AbortError') return
      setReadError(reason instanceof Error ? reason.message : String(reason))
    }).finally(() => setReading(false))
    void load()
    void listVirtualGrantees(controller.signal).then(setGrantees).catch((reason: unknown) => {
      if (!(reason instanceof DOMException && reason.name === 'AbortError')) setReadError(reason instanceof Error ? reason.message : String(reason))
    })
    const timer = window.setInterval(() => void load(), 10000)
    return () => { controller.abort(); window.clearInterval(timer) }
  }, [reload])

  function select(id: string) {
    openAccount(accounts.find((account) => account.virtual_id === id))
    setError(null)
    setNotice(null)
  }
  const available = positions.filter((position) => !bindings.some((binding) => binding.position_strategy_name === position.strategy_name))
  const validShare = (binding: VirtualBinding) => {
    const value = shareInputs[binding.binding_name] ?? String(binding.shares)
    return value.trim() !== '' && Number.isFinite(Number(value)) && Number(value) >= 0
  }
  const validShares = bindings.every(validShare)
  const selectedAccount = accounts.find((account) => account.virtual_id === selected)
  const canEdit = !selected || selectedAccount?.can_configure === true
  const serverGrantIds = selectedAccount?.managers.map((manager) => manager.user_id).sort((a, b) => a - b).join(',') ?? ''
  useEffect(() => { setGrantUserIds(serverGrantIds ? serverGrantIds.split(',').map(Number) : []) }, [selected, serverGrantIds])
  const grantsDirty = [...grantUserIds].sort((a, b) => a - b).join(',') !== serverGrantIds
  const nextBindings = bindings.map((binding) => ({ ...binding, shares: Number(shareInputs[binding.binding_name] ?? binding.shares) }))
  const dirty = selectedAccount
    ? name.trim() !== selectedAccount.name || JSON.stringify(nextBindings) !== JSON.stringify(selectedAccount.bindings)
    : !!name.trim() || bindings.length > 0
  const filtered = accounts.filter((account) => `${account.name} ${account.virtual_id}`.toLowerCase().includes(query.trim().toLowerCase()))
  const followerCount = new Set(accounts.flatMap((account) => account.followers.map((follower) => follower.source_id))).size
  const locked = !canEdit || saving || loading || reading

  return <AppShell active="virtual" title="Virtual 账户" subtitle="策略组合管理工作台" icon={Layers3}>
    <PageIntro eyebrow="Virtual Accounts" title="Virtual 账户管理" description="维护策略组合，在实际账户的策略启用页选择跟随账户与倍率。" actions={<div className="flex flex-wrap gap-2">
      <Button type="button" disabled={saving || reading} onClick={() => void withWrite(async () => { await reload(); return '列表与同步状态已刷新。' })}><RefreshCw size={15} /> 刷新</Button>
      <Button type="button" variant="primary" disabled={saving || reading} onClick={() => select('')}><Plus size={15} /> 新建账户</Button>
    </div>} />
    {(readError ?? catalogError ?? writeError) && <Alert className="mb-4" tone="error">{readError ?? catalogError ?? writeError}</Alert>}
    {notice && <Alert className="mb-4" tone="success">{notice}</Alert>}
    {!canEdit && <p className="mb-4 text-sm text-muted">当前组合仅可查看，可向创建者申请管理授权。你也可以新建自己的 Virtual 账户。</p>}

    <div className="grid items-start gap-5 lg:grid-cols-[260px_minmax(0,1fr)]">
      <Card className="min-w-0">
        <CardHeader>
          <CardTitle className="flex items-center justify-between gap-2">账户列表<Badge>{reading ? '…' : accounts.length}</Badge></CardTitle>
          <CardDescription>{reading ? '正在加载账户' : `${accounts.reduce((sum, account) => sum + account.bindings.length, 0)} 条策略绑定 · ${followerCount} 个跟随账户`}</CardDescription>
        </CardHeader>
        <CardContent className="space-y-3">
          <Input aria-label="搜索 Virtual 账户" placeholder="搜索编号或别名" value={query} onChange={(event) => setQuery(event.target.value)} />
          <div className="max-h-64 space-y-2 overflow-y-auto lg:max-h-[36rem]">
            {reading ? <p className="py-4 text-sm text-muted">正在加载…</p> : filtered.length === 0 ? <p className="py-4 text-sm text-muted">{accounts.length ? '没有匹配的账户。' : '暂无账户，点击「新建账户」创建。'}</p> : filtered.map((account) => <button type="button" key={account.virtual_id} disabled={saving} onClick={() => select(account.virtual_id)} aria-pressed={selected === account.virtual_id}
              className={cn('w-full min-w-0 rounded-xl border p-3 text-left transition-colors', selected === account.virtual_id ? 'border-brand-ring bg-brand-soft' : 'border-border hover:bg-canvas')}>
              <span className="block break-all font-mono text-xs text-muted">{account.virtual_id}</span>
              <span className="mt-1 block truncate text-sm font-semibold text-ink" title={account.name}>{account.name}</span>
              <span className="mt-2 block text-xs text-muted">{account.bindings.length} 条策略 · {account.followers.length} 个跟随账户</span>
            </button>)}
          </div>
        </CardContent>
      </Card>

      <div className="min-w-0 space-y-5">
        <Card className="min-w-0">
          <CardHeader>
            <div className="flex flex-wrap items-center justify-between gap-2">
              <CardTitle className="min-w-0 break-words">{selectedAccount ? selectedAccount.name : '新建 Virtual 账户'}</CardTitle>
              {canEdit && <Badge tone={dirty ? 'warning' : 'neutral'}>{selected ? dirty ? '有未保存修改' : '已保存' : '新建草稿'}</Badge>}
            </div>
            <CardDescription>配置别名、仓位策略、下单模板和每份组合的份数。</CardDescription>
          </CardHeader>
          <CardContent>
            <form onSubmit={(event) => {
              event.preventDefault()
              if (locked || !name.trim() || !validShares) return
              void withWrite(async () => {
                const saved = selected
                  ? await saveVirtualAccount(selected, name.trim(), nextBindings)
                  : await createVirtualAccount(name.trim(), nextBindings)
                setAccounts(await listVirtualAccounts())
                openAccount(saved)
                return '组合已保存，跟随账户将自动同步。'
              })
            }}>
              <fieldset disabled={locked} className="min-w-0 space-y-6">
                <div className="grid gap-4 sm:grid-cols-[minmax(0,180px)_minmax(0,1fr)]">
                  <div className="min-w-0 space-y-1.5">
                    <p className="text-xs font-medium text-muted">账户编号</p>
                    <p className="flex min-h-9 items-center break-all font-mono text-sm text-ink">{selected || '保存后自动分配'}</p>
                    <FieldHint>{selected ? '编号固定，别名可修改。' : '按 virtual01、virtual02 顺序分配。'}</FieldHint>
                  </div>
                  <Label className="min-w-0">别名<Input value={name} placeholder="例如：趋势组合" maxLength={200} onChange={(event) => setName(event.target.value)} required /><FieldHint>显示在账户列表和跟随选项中。</FieldHint></Label>
                </div>

                <div className="space-y-3 border-t border-border-soft pt-5">
                  <div className="flex flex-wrap items-center gap-2"><h4 className="text-sm font-semibold text-ink">策略组合</h4><Badge>{bindings.length} 条</Badge></div>
                  <FieldHint>实际账户生效份数 = Virtual 份数 × 跟随倍率。份数设为 0 可停止该策略。</FieldHint>
                  {bindings.length === 0 ? <div className="rounded-xl border border-dashed border-border bg-canvas/50 px-4 py-6 text-center">
                    <p className="text-sm font-medium text-muted">尚未添加策略</p>
                    <p className="mt-1 text-xs text-subtle">从下方选择仓位策略，添加到组合中。</p>
                  </div> : bindings.map((binding, index) => <div key={binding.binding_name} className="min-w-0 rounded-xl border border-border bg-canvas/30 p-4">
                    <div className="mb-4 flex items-start justify-between gap-3">
                      <div className="min-w-0">
                        <p className="text-xs text-muted">仓位策略</p>
                        <p className="mt-1 break-all text-sm font-semibold text-ink">{binding.position_strategy_name}</p>
                        {binding.binding_name !== binding.position_strategy_name && <p className="mt-1 break-all text-xs text-subtle">发布名：{binding.binding_name}</p>}
                      </div>
                      {canEdit && <Button type="button" variant="ghost" size="sm" className="shrink-0" aria-label={`移除 ${binding.position_strategy_name}`} onClick={() => setBindings((old) => old.filter((_, i) => i !== index))}><Trash2 size={14} /> 移除</Button>}
                    </div>
                    <div className="grid items-start gap-3 sm:grid-cols-[minmax(0,1fr)_9rem]">
                      <Label className="min-w-0">下单模板<Select className="min-w-0" value={binding.order_strategy_name} onChange={(event) => setBindings((old) => old.map((item, i) => i === index ? { ...item, order_strategy_name: event.target.value } : item))}>
                        {!orders.some((order) => order.strategy_name === binding.order_strategy_name) && <option value={binding.order_strategy_name}>{binding.order_strategy_name}</option>}
                        {orders.map((order) => <option key={order.strategy_name} value={order.strategy_name}>{order.strategy_name} ({order.order_parameters.algorithm.toUpperCase()})</option>)}
                      </Select></Label>
                      <Label className="min-w-0">Virtual 份数<Input inputMode="decimal" aria-invalid={!validShare(binding)} value={shareInputs[binding.binding_name] ?? String(binding.shares)} onChange={(event) => setShareInputs((old) => ({ ...old, [binding.binding_name]: event.target.value }))} />{!validShare(binding) && <FieldHint className="text-danger">请输入不小于 0 的数字。</FieldHint>}</Label>
                    </div>
                  </div>)}
                  {canEdit && <div className="flex flex-col items-stretch gap-3 rounded-xl bg-canvas p-3 sm:flex-row sm:items-end">
                    <Label className="min-w-0 flex-1">添加仓位策略<Select className="min-w-0" value={strategyToAdd} onChange={(event) => setStrategyToAdd(event.target.value)} disabled={!orders.length || !available.length}>
                      <option value="">{!orders.length ? '请先创建下单模板' : !available.length ? '没有可添加的仓位策略' : '选择仓位策略'}</option>
                      {available.map((position) => <option key={position.strategy_name} value={position.strategy_name}>{position.strategy_name}</option>)}
                    </Select></Label>
                    <Button type="button" className="shrink-0" disabled={!strategyToAdd || !orders.length} onClick={() => {
                      if (!orders[0] || !available.some((position) => position.strategy_name === strategyToAdd)) return
                      setBindings((old) => [...old, { binding_name: strategyToAdd, position_strategy_name: strategyToAdd, order_strategy_name: orders[0].strategy_name, shares: 1 }])
                      setShareInputs((old) => ({ ...old, [strategyToAdd]: '1' }))
                      setStrategyToAdd('')
                    }}><Plus size={15} /> 添加</Button>
                  </div>}
                </div>

                {selectedAccount && selectedAccount.bindings.length > 0 && bindings.length === 0 && <Alert tone="warning">保存空组合将停止跟随账户中原有的全部策略。</Alert>}
                <FieldHint>保存后自动更新全部跟随账户。移除的策略会同步停止。</FieldHint>
                {canEdit && <div className="flex flex-col gap-3 border-t border-border-soft pt-4 sm:flex-row sm:flex-wrap sm:items-center sm:justify-between">
                  <div className="flex flex-wrap gap-2">
                    <Button type="submit" variant="primary" disabled={!name.trim() || !validShares || (!!selected && !dirty)}><Save size={15} /> {saving ? '正在保存…' : selected ? '保存并同步' : '创建账户'}</Button>
                    <Button type="button" disabled={!dirty && !!selected} onClick={() => select(selected || accounts[0]?.virtual_id || '')}>{selected ? '撤销修改' : '取消新建'}</Button>
                  </div>
                  {selectedAccount && <Button type="button" variant="danger" disabled={selectedAccount.followers.length > 0} title={selectedAccount.followers.length ? '请先将跟随账户切回独立配置' : undefined} onClick={() => void withWrite(async () => {
                    await deleteVirtualAccount(selected)
                    const next = await listVirtualAccounts()
                    setAccounts(next)
                    openAccount(next[0])
                    return 'Virtual 账户已删除。'
                  })}><Trash2 size={15} /> 删除账户</Button>}
                </div>}
              </fieldset>
            </form>
          </CardContent>
        </Card>

        {selectedAccount && <Card className="min-w-0">
          <CardHeader>
            <CardTitle>管理授权</CardTitle>
            <CardDescription>创建者：{selectedAccount.owner_username ?? '管理员维护'}{selectedAccount.created_by_user_id === user.user_id ? '（你）' : ''}</CardDescription>
          </CardHeader>
          <CardContent className="space-y-4">
            <FieldHint>获授权用户可修改和删除此 Virtual。创建者与管理员可以授予或撤销管理权限；实际账户和仓位策略仍使用各自的权限。</FieldHint>
            {selectedAccount.can_manage_grants ? <>
              <div className="grid gap-2 sm:grid-cols-2">
                {grantees.filter((grantee) => grantee.user_id !== selectedAccount.created_by_user_id).map((grantee) => <label key={grantee.user_id} className="flex min-w-0 items-center gap-3 rounded-lg border border-border px-3 py-2 text-sm">
                  <input type="checkbox" disabled={saving} checked={grantUserIds.includes(grantee.user_id)} onChange={(event) => setGrantUserIds((ids) => event.target.checked ? [...ids, grantee.user_id] : ids.filter((id) => id !== grantee.user_id))} />
                  <span className="break-all">{grantee.username}</span>
                </label>)}
              </div>
              <Button type="button" disabled={saving || !grantsDirty} onClick={() => void withWrite(async () => {
                await saveVirtualGrants(selectedAccount.virtual_id, grantUserIds)
                await reload()
                return '管理授权已保存。'
              })}><Save size={15} /> 保存授权</Button>
            </> : <p className="text-sm text-muted">获授权用户：{selectedAccount.managers.map((manager) => manager.username).join('、') || '暂无'}</p>}
          </CardContent>
        </Card>}

        {selectedAccount && <Card className="min-w-0">
          <CardHeader>
            <CardTitle className="flex items-center gap-2"><Users size={16} /> 跟随账户<Badge>{selectedAccount.followers.length}</Badge></CardTitle>
            <CardDescription>组合更新：{timestampUs(selectedAccount.updated_at_us)} · 同步状态每 10 秒刷新</CardDescription>
          </CardHeader>
          <CardContent>
            {selectedAccount.followers.length === 0 ? <p className="text-sm leading-relaxed text-muted">暂无跟随账户。在实际账户的策略启用页选择「跟随 Virtual」，再设置倍率。</p> : <div className="grid gap-3 xl:grid-cols-2">{selectedAccount.followers.map((follower) => <div key={follower.source_id} className="min-w-0 rounded-xl border border-border p-4">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <a href={routes.configBindings(follower.source_id)} className="min-w-0 break-all text-sm font-medium text-brand">{follower.source_id} →</a>
                <Badge tone={follower.pending_publishes.length ? 'warning' : 'success'}>{follower.pending_publishes.length ? `待同步 ${follower.pending_publishes.length} 条` : '已同步'}</Badge>
              </div>
              <p className="mt-2 text-sm text-muted">跟随倍率：{follower.multiplier}x</p>
              {follower.pending_publishes.map((pending) => <p key={pending.binding_name} className="mt-1 break-words text-xs text-muted">{pending.binding_name}：{pending.error ?? '等待发布'}</p>)}
            </div>)}</div>}
            {selectedAccount.followers.length > 0 && canEdit && <FieldHint className="mt-3">删除此 Virtual 账户前，请先将以上跟随账户切回独立配置。</FieldHint>}
          </CardContent>
        </Card>}
      </div>
    </div>
  </AppShell>
}
