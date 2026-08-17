import {
  CheckCircle2,
  Layers3,
  Link2,
  LoaderCircle,
  Plus,
  RefreshCw,
  Save,
  Settings2,
  Trash2,
  Wallet,
} from 'lucide-react'
import { useCallback, useEffect, useMemo, useState } from 'react'
import {
  ApiError,
  deleteAccountBinding,
  deleteOrderStrategy,
  deletePositionStrategy,
  getAccountStudio,
  getDashboard,
  listOrderStrategies,
  listPositionStrategies,
  publishAccountBinding,
  saveAccountBinding,
  saveAccountStudio,
  saveOrderStrategy,
  savePositionStrategy,
} from '../api'
import { AppShell, PageIntro, StatTile } from '../components/AppShell'
import { TargetPositionsEditor } from '../components/TargetPositionsEditor'
import { Alert, Badge } from '../components/ui/Badge'
import { Button } from '../components/ui/Button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '../components/ui/Card'
import { FieldHint, Input, Label, Select } from '../components/ui/Field'
import { Tabs } from '../components/ui/Tabs'
import { money } from '../format'
import { cn } from '../lib/cn'
import type {
  AccountStudio,
  CatalogOrderStrategy,
  DashboardSnapshot,
  OrderParameters,
  PositionStrategy,
} from '../types'

type StudioTab = 'position' | 'order' | 'account'

const DEFAULT_ORDER: OrderParameters = {
  single_order_usdt: 100,
  orders_per_batch: 3,
  maker_price_anchor: 'own_best',
  tick_spacing: 1,
  batch_interval_ms: 500,
  maker_timeout_ms: 1000,
  max_maker_requotes: 2,
  target_tolerance_usdt: 10,
}

const TAB_ITEMS: Array<{ id: StudioTab; label: string; hint: string }> = [
  { id: 'position', label: '仓位策略', hint: '目标仓位与参考权益' },
  { id: 'order', label: '下单策略', hint: '执行参数模板' },
  { id: 'account', label: '账户组合', hint: '绑定与发布' },
]

function emptyPosition(): PositionStrategy {
  return { strategy_name: '', equity_usdt: 10_000, targets: {}, updated_at_us: 0 }
}

function emptyOrder(): CatalogOrderStrategy {
  return { strategy_name: '', order_parameters: { ...DEFAULT_ORDER }, updated_at_us: 0 }
}

function percent(ratio: number) {
  return `${(ratio * 100).toFixed(1)}%`
}

function nextAllocationRatio(studio: AccountStudio, bindingName: string, nextEquity: number) {
  const replaced =
    studio.bindings.find((binding) => binding.binding_name === bindingName)?.position_equity_usdt ?? 0
  const total = studio.bound_equity_usdt - replaced + nextEquity
  return total > 0 ? nextEquity / total : 0
}

function StrategyPicker({
  title,
  emptyLabel,
  items,
  selectedName,
  onSelect,
  onCreate,
  renderMeta,
}: {
  title: string
  emptyLabel: string
  items: Array<{ strategy_name: string }>
  selectedName: string
  onSelect: (name: string) => void
  onCreate: () => void
  renderMeta: (name: string) => string
}) {
  return (
    <Card className="h-fit">
      <CardHeader className="flex flex-row items-start justify-between gap-3">
        <div>
          <CardTitle>{title}</CardTitle>
          <CardDescription>{items.length} 个已保存</CardDescription>
        </div>
        <Button type="button" size="sm" variant="primary" onClick={onCreate}>
          <Plus size={14} /> 新建
        </Button>
      </CardHeader>
      <CardContent className="space-y-2 pt-0">
        {items.length === 0 ? (
          <div className="rounded-xl border border-dashed border-border px-4 py-8 text-center text-sm text-muted">
            {emptyLabel}
          </div>
        ) : (
          items.map((item) => {
            const active = selectedName === item.strategy_name
            return (
              <button
                key={item.strategy_name}
                type="button"
                onClick={() => onSelect(item.strategy_name)}
                className={cn(
                  'w-full rounded-xl border px-3 py-3 text-left transition-all',
                  active
                    ? 'border-brand bg-brand-soft shadow-sm'
                    : 'border-border-soft bg-canvas/40 hover:border-border hover:bg-surface',
                )}
              >
                <p className="truncate text-sm font-medium text-ink">{item.strategy_name}</p>
                <p className="mt-1 text-xs text-muted">{renderMeta(item.strategy_name)}</p>
              </button>
            )
          })
        )}
      </CardContent>
    </Card>
  )
}

export function ConfigPage() {
  const [tab, setTab] = useState<StudioTab>('position')
  const [dashboard, setDashboard] = useState<DashboardSnapshot | null>(null)
  const [positions, setPositions] = useState<PositionStrategy[]>([])
  const [orders, setOrders] = useState<CatalogOrderStrategy[]>([])
  const [studio, setStudio] = useState<AccountStudio | null>(null)
  const [sourceId, setSourceId] = useState('')
  const [selectedPosition, setSelectedPosition] = useState<PositionStrategy>(emptyPosition)
  const [selectedOrder, setSelectedOrder] = useState<CatalogOrderStrategy>(emptyOrder)
  const [leverage, setLeverage] = useState('1')
  const [bindingName, setBindingName] = useState('')
  const [bindPosition, setBindPosition] = useState('')
  const [bindOrder, setBindOrder] = useState('')
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)

  const accounts = useMemo(
    () => (dashboard?.accounts ?? []).filter((account) => account.enabled && account.configurable),
    [dashboard],
  )

  const reloadCatalog = useCallback(async (signal?: AbortSignal) => {
    const [nextPositions, nextOrders] = await Promise.all([
      listPositionStrategies(signal),
      listOrderStrategies(signal),
    ])
    setPositions(nextPositions)
    setOrders(nextOrders)
    setBindPosition((current) =>
      nextPositions.some((item) => item.strategy_name === current)
        ? current
        : (nextPositions[0]?.strategy_name ?? ''),
    )
    setBindOrder((current) =>
      nextOrders.some((item) => item.strategy_name === current)
        ? current
        : (nextOrders[0]?.strategy_name ?? ''),
    )
  }, [])

  useEffect(() => {
    const controller = new AbortController()
    Promise.all([getDashboard(controller.signal), reloadCatalog(controller.signal)])
      .then(([snapshot]) => {
        setDashboard(snapshot)
        setError(null)
      })
      .catch((reason: unknown) => {
        if (reason instanceof DOMException && reason.name === 'AbortError') return
        setError(reason instanceof Error ? reason.message : String(reason))
      })
      .finally(() => setLoading(false))
    return () => controller.abort()
  }, [reloadCatalog])

  useEffect(() => {
    if (!accounts.length) return
    if (!accounts.some((account) => account.source_id === sourceId)) {
      setSourceId(accounts[0].source_id)
    }
  }, [accounts, sourceId])

  useEffect(() => {
    if (!sourceId) {
      setStudio(null)
      return
    }
    const controller = new AbortController()
    getAccountStudio(sourceId, controller.signal)
      .then((next) => {
        setStudio(next)
        setLeverage(String(next.leverage))
        setError(null)
      })
      .catch((reason: unknown) => {
        if (reason instanceof DOMException && reason.name === 'AbortError') return
        setError(reason instanceof Error ? reason.message : String(reason))
      })
    return () => controller.abort()
  }, [sourceId])

  async function withWrite<T>(action: () => Promise<T>) {
    setSaving(true)
    setError(null)
    setNotice(null)
    try {
      const result = await action()
      setNotice('已保存')
      return result
    } catch (reason: unknown) {
      setError(reason instanceof ApiError ? reason.message : String(reason))
    } finally {
      setSaving(false)
    }
  }

  return (
    <AppShell
      active="config"
      title="CTA Manager"
      subtitle="策略组合工作室"
      icon={Settings2}
      actions={
        saving ? (
          <Badge tone="brand" className="hidden sm:inline-flex">
            <RefreshCw size={12} className="mr-1 animate-spin-slow" /> 写入中
          </Badge>
        ) : null
      }
    >
      <PageIntro
        eyebrow="Strategy Studio"
        title="组合仓位、下单与账户绑定"
        description="仓位策略和下单策略独立维护，账户只负责选择组合并按参考权益计算分配比例。保存后点发布，才会写入 Exec。"
      />

      <div className="space-y-4">
        {error && <Alert tone="error">{error}</Alert>}
        {notice && <Alert tone="success">{notice}</Alert>}

        <Tabs value={tab} onChange={setTab} items={TAB_ITEMS} />

        {loading ? (
          <Card>
            <CardContent className="flex items-center justify-center gap-2 py-16 text-sm text-muted">
              <LoaderCircle size={18} className="animate-spin-slow" />
              正在加载策略目录
            </CardContent>
          </Card>
        ) : tab === 'position' ? (
          <div className="grid gap-6 lg:grid-cols-[300px_minmax(0,1fr)]">
            <StrategyPicker
              title="仓位策略库"
              emptyLabel="还没有仓位策略，先创建一个。"
              items={positions}
              selectedName={selectedPosition.strategy_name}
              onSelect={(name) => {
                const item = positions.find((entry) => entry.strategy_name === name)
                if (!item) return
                setSelectedPosition(item)
              }}
              onCreate={() => {
                setSelectedPosition(emptyPosition())
              }}
              renderMeta={(name) => {
                const item = positions.find((entry) => entry.strategy_name === name)
                if (!item) return ''
                const active = Object.values(item.targets).filter((value) => value !== 0).length
                return `参考权益 ${money(item.equity_usdt)} USDT · ${active} 非零`
              }}
            />

            <Card>
              <CardHeader>
                <CardTitle>编辑仓位策略</CardTitle>
                <CardDescription>按参考权益定义各品种目标仓位，绑定账户后仅参与比例计算。</CardDescription>
              </CardHeader>
              <CardContent>
                <form
                  className="grid gap-4"
                  onSubmit={(event) => {
                    event.preventDefault()
                    void withWrite(async () => {
                      const saved = await savePositionStrategy({
                        ...selectedPosition,
                        equity_usdt: Number(selectedPosition.equity_usdt),
                        targets: selectedPosition.targets,
                      })
                      setSelectedPosition(saved)
                      await reloadCatalog()
                    })
                  }}
                >
                  <div className="grid gap-4 md:grid-cols-2">
                    <Label>
                      策略名
                      <Input
                        value={selectedPosition.strategy_name}
                        onChange={(event) =>
                          setSelectedPosition({
                            ...selectedPosition,
                            strategy_name: event.target.value,
                          })
                        }
                      />
                    </Label>
                    <Label>
                      参考权益 USDT
                      <Input
                        type="number"
                        min="1"
                        step="1"
                        value={selectedPosition.equity_usdt}
                        onChange={(event) =>
                          setSelectedPosition({
                            ...selectedPosition,
                            equity_usdt: Number(event.target.value),
                          })
                        }
                      />
                    </Label>
                  </div>
                  <FieldHint>默认 10000 USDT。账户实时权益变动时，这里只用于计算组合比例。</FieldHint>
                  <TargetPositionsEditor
                    targets={selectedPosition.targets}
                    onChange={(targets) =>
                      setSelectedPosition({
                        ...selectedPosition,
                        targets,
                      })
                    }
                  />
                  <div className="flex flex-wrap gap-2">
                    <Button type="submit" variant="primary" disabled={saving}>
                      <Save size={15} /> 保存仓位策略
                    </Button>
                    {selectedPosition.strategy_name && (
                      <Button
                        type="button"
                        variant="danger"
                        disabled={saving}
                        onClick={() =>
                          void withWrite(async () => {
                            await deletePositionStrategy(selectedPosition.strategy_name)
                            setSelectedPosition(emptyPosition())
                            await reloadCatalog()
                          })
                        }
                      >
                        <Trash2 size={15} /> 删除
                      </Button>
                    )}
                  </div>
                </form>
              </CardContent>
            </Card>
          </div>
        ) : tab === 'order' ? (
          <div className="grid gap-6 lg:grid-cols-[300px_minmax(0,1fr)]">
            <StrategyPicker
              title="下单策略库"
              emptyLabel="还没有下单策略，先创建一个。"
              items={orders}
              selectedName={selectedOrder.strategy_name}
              onSelect={(name) => {
                const item = orders.find((entry) => entry.strategy_name === name)
                if (item) setSelectedOrder(item)
              }}
              onCreate={() => setSelectedOrder(emptyOrder())}
              renderMeta={(name) => {
                const item = orders.find((entry) => entry.strategy_name === name)
                return item ? `单笔 ${item.order_parameters.single_order_usdt} USDT` : ''
              }}
            />

            <Card>
              <CardHeader>
                <CardTitle>编辑下单策略</CardTitle>
                <CardDescription>8 个执行参数可复用到多个账户绑定。</CardDescription>
              </CardHeader>
              <CardContent>
                <form
                  className="grid gap-4"
                  onSubmit={(event) => {
                    event.preventDefault()
                    void withWrite(async () => {
                      const saved = await saveOrderStrategy(selectedOrder)
                      setSelectedOrder(saved)
                      await reloadCatalog()
                    })
                  }}
                >
                  <Label>
                    策略名
                    <Input
                      value={selectedOrder.strategy_name}
                      onChange={(event) =>
                        setSelectedOrder({ ...selectedOrder, strategy_name: event.target.value })
                      }
                    />
                  </Label>
                  <div className="grid gap-4 sm:grid-cols-2">
                    {(
                      [
                        ['single_order_usdt', '单笔名义金额'],
                        ['orders_per_batch', '每批订单数'],
                        ['tick_spacing', 'Tick 间距'],
                        ['batch_interval_ms', '批次间隔 ms'],
                        ['maker_timeout_ms', 'Maker 超时 ms'],
                        ['max_maker_requotes', '最大重报价'],
                        ['target_tolerance_usdt', '目标容差 USDT'],
                      ] as const
                    ).map(([field, label]) => (
                      <Label key={field}>
                        {label}
                        <Input
                          type="number"
                          value={selectedOrder.order_parameters[field]}
                          onChange={(event) =>
                            setSelectedOrder({
                              ...selectedOrder,
                              order_parameters: {
                                ...selectedOrder.order_parameters,
                                [field]: Number(event.target.value),
                              },
                            })
                          }
                        />
                      </Label>
                    ))}
                  </div>
                  <Label>
                    Maker 价格锚点
                    <Select
                      value={selectedOrder.order_parameters.maker_price_anchor}
                      onChange={(event) =>
                        setSelectedOrder({
                          ...selectedOrder,
                          order_parameters: {
                            ...selectedOrder.order_parameters,
                            maker_price_anchor: event.target
                              .value as OrderParameters['maker_price_anchor'],
                          },
                        })
                      }
                    >
                      <option value="own_best">己方一档</option>
                      <option value="opposite_best_plus_one_tick">对手一档 + 1 tick</option>
                    </Select>
                  </Label>
                  <div className="flex flex-wrap gap-2">
                    <Button type="submit" variant="primary" disabled={saving}>
                      <Save size={15} /> 保存下单策略
                    </Button>
                    {selectedOrder.strategy_name && (
                      <Button
                        type="button"
                        variant="danger"
                        disabled={saving}
                        onClick={() =>
                          void withWrite(async () => {
                            await deleteOrderStrategy(selectedOrder.strategy_name)
                            setSelectedOrder(emptyOrder())
                            await reloadCatalog()
                          })
                        }
                      >
                        <Trash2 size={15} /> 删除
                      </Button>
                    )}
                  </div>
                </form>
              </CardContent>
            </Card>
          </div>
        ) : (
          <div className="space-y-6">
            <div className="grid gap-4 lg:grid-cols-[minmax(0,1fr)_320px]">
              <Card>
                <CardHeader>
                  <CardTitle className="flex items-center gap-2">
                    <Wallet size={16} /> 账户设置
                  </CardTitle>
                  <CardDescription>账户权益实时变化，这里只保存杠杆与绑定关系。</CardDescription>
                </CardHeader>
                <CardContent className="grid gap-4 sm:grid-cols-2">
                  <Label>
                    账户
                    <Select value={sourceId} onChange={(event) => setSourceId(event.target.value)}>
                      {accounts.map((account) => (
                        <option key={account.source_id} value={account.source_id}>
                          {account.account} / {account.source_id}
                        </option>
                      ))}
                    </Select>
                  </Label>
                  <Label>
                    杠杆率
                    <Input value={leverage} onChange={(event) => setLeverage(event.target.value)} />
                  </Label>
                  <div className="sm:col-span-2">
                    <Button
                      type="button"
                      variant="primary"
                      disabled={saving}
                      onClick={() =>
                        void withWrite(async () => {
                          const next = await saveAccountStudio(sourceId, Number(leverage))
                          setStudio(next)
                        })
                      }
                    >
                      <Save size={15} /> 保存杠杆
                    </Button>
                  </div>
                </CardContent>
              </Card>

              {studio && (
                <div className="grid gap-3">
                  <StatTile label="杠杆" value={String(studio.leverage)} />
                  <StatTile
                    label="参考权益合计"
                    value={`${money(studio.bound_equity_usdt)} USDT`}
                    hint="各绑定仓位策略参考权益之和"
                  />
                  <StatTile label="已绑定" value={`${studio.bindings.length} 组`} />
                </div>
              )}
            </div>

            <div className="grid gap-6 xl:grid-cols-[360px_minmax(0,1fr)]">
              <Card>
                <CardHeader>
                  <CardTitle className="flex items-center gap-2">
                    <Link2 size={16} /> 新建绑定
                  </CardTitle>
                  <CardDescription>选择仓位策略 × 下单策略，并指定 Exec 发布名。</CardDescription>
                </CardHeader>
                <CardContent>
                  <form
                    className="grid gap-4"
                    onSubmit={(event) => {
                      event.preventDefault()
                      void withWrite(async () => {
                        const next = await saveAccountBinding(
                          sourceId,
                          bindingName,
                          bindPosition,
                          bindOrder,
                        )
                        setStudio(next)
                        setBindingName('')
                      })
                    }}
                  >
                    <Label>
                      发布名
                      <Input
                        value={bindingName}
                        placeholder="写入 Exec 的策略名"
                        onChange={(event) => setBindingName(event.target.value)}
                      />
                    </Label>
                    <Label>
                      仓位策略
                      <Select
                        value={bindPosition}
                        onChange={(event) => setBindPosition(event.target.value)}
                      >
                        {positions.map((item) => (
                          <option key={item.strategy_name} value={item.strategy_name}>
                            {item.strategy_name}
                          </option>
                        ))}
                      </Select>
                    </Label>
                    <Label>
                      下单策略
                      <Select value={bindOrder} onChange={(event) => setBindOrder(event.target.value)}>
                        {orders.map((item) => (
                          <option key={item.strategy_name} value={item.strategy_name}>
                            {item.strategy_name}
                          </option>
                        ))}
                      </Select>
                    </Label>
                    {studio && bindPosition && (
                      <FieldHint>
                        预计占比{' '}
                        {percent(
                          nextAllocationRatio(
                            studio,
                            bindingName,
                            positions.find((item) => item.strategy_name === bindPosition)
                              ?.equity_usdt ?? 0,
                          ),
                        )}
                      </FieldHint>
                    )}
                    <Button type="submit" variant="primary" disabled={saving}>
                      <Plus size={15} /> 绑定到账户
                    </Button>
                  </form>
                </CardContent>
              </Card>

              <div className="space-y-4">
                <div className="flex items-center gap-2 text-sm font-medium text-ink">
                  <Layers3 size={16} className="text-brand" />
                  当前绑定
                </div>
                {(studio?.bindings ?? []).length === 0 ? (
                  <Card>
                    <CardContent className="py-12 text-center text-sm text-muted">
                      还没有绑定组合。先从左侧创建绑定。
                    </CardContent>
                  </Card>
                ) : (
                  (studio?.bindings ?? []).map((binding) => (
                    <Card key={binding.binding_name}>
                      <CardContent className="space-y-4">
                        <div className="flex flex-wrap items-start justify-between gap-3">
                          <div>
                            <p className="text-base font-semibold text-ink">{binding.binding_name}</p>
                            <p className="mt-1 text-sm text-muted">
                              {binding.position_strategy_name} × {binding.order_strategy_name}
                            </p>
                          </div>
                          <Badge tone="brand">{percent(binding.allocation_ratio)}</Badge>
                        </div>
                        <div className="h-2 overflow-hidden rounded-full bg-canvas">
                          <div
                            className="h-full rounded-full bg-brand transition-all"
                            style={{ width: `${Math.max(binding.allocation_ratio * 100, 4)}%` }}
                          />
                        </div>
                        <p className="text-xs text-subtle">
                          参考权益 {money(binding.position_equity_usdt)} USDT
                        </p>
                        <div className="flex flex-wrap gap-2">
                          <Button
                            type="button"
                            variant="primary"
                            disabled={saving}
                            onClick={() =>
                              void withWrite(async () => {
                                await publishAccountBinding(sourceId, binding.binding_name)
                              })
                            }
                          >
                            <CheckCircle2 size={15} /> 发布到 Exec
                          </Button>
                          <Button
                            type="button"
                            variant="ghost"
                            disabled={saving}
                            onClick={() =>
                              void withWrite(async () => {
                                await deleteAccountBinding(sourceId, binding.binding_name)
                                const next = await getAccountStudio(sourceId)
                                setStudio(next)
                              })
                            }
                          >
                            <Trash2 size={15} /> 解绑
                          </Button>
                        </div>
                      </CardContent>
                    </Card>
                  ))
                )}
              </div>
            </div>
          </div>
        )}
      </div>
    </AppShell>
  )
}
