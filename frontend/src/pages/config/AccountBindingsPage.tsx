import { CheckCircle2, Layers3, LoaderCircle, Plus, SlidersHorizontal, Trash2 } from 'lucide-react'
import { useCallback, useEffect, useMemo, useState } from 'react'
import {
  deleteAccountBinding,
  getAccountLive,
  getAccountStudio,
  getDashboard,
  publishAccountBinding,
  saveAccountAllocations,
  saveAccountBinding,
  saveAccountLeverage,
} from '../../api'
import { AllocationEditor } from '../../components/AllocationEditor'
import { CapacityPanel, LeverageToolbar } from '../../components/CapacityPanel'
import { ConfigShell } from '../../components/ConfigShell'
import { Alert } from '../../components/ui/Badge'
import { Button } from '../../components/ui/Button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '../../components/ui/Card'
import { FieldHint, Input, Label, Select } from '../../components/ui/Field'
import { useConfigWrite } from '../../hooks/useConfigWrite'
import { useStrategyCatalog } from '../../hooks/useStrategyCatalog'
import { money } from '../../format'
import { percent } from '../../lib/strategyDefaults'
import { readSourceId, routes } from '../../lib/routes'
import type { AccountCapacity, AccountStudio, DashboardSnapshot } from '../../types'

export function AccountBindingsPage() {
  const initialSource = readSourceId()
  const { positions, orders, loading: catalogLoading, error: catalogError, reloadCatalog } =
    useStrategyCatalog()
  const { saving, error: writeError, notice, withWrite } = useConfigWrite()
  const [dashboard, setDashboard] = useState<DashboardSnapshot | null>(null)
  const [studio, setStudio] = useState<AccountStudio | null>(null)
  const [capacity, setCapacity] = useState<AccountCapacity | null>(null)
  const [sourceId, setSourceId] = useState(initialSource)
  const [leverage, setLeverage] = useState('1')
  const [newPosition, setNewPosition] = useState('')
  const [newOrder, setNewOrder] = useState('')
  const [newShares, setNewShares] = useState('1')
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const accounts = useMemo(
    () => (dashboard?.accounts ?? []).filter((account) => account.enabled && account.configurable),
    [dashboard],
  )

  const boundNames = useMemo(
    () => new Set((studio?.bindings ?? []).map((binding) => binding.position_strategy_name)),
    [studio],
  )

  const availablePositions = useMemo(
    () => positions.filter((item) => !boundNames.has(item.strategy_name)),
    [boundNames, positions],
  )

  useEffect(() => {
    const controller = new AbortController()
    getDashboard(controller.signal)
      .then((snapshot) => {
        setDashboard(snapshot)
        setError(null)
      })
      .catch((reason: unknown) => {
        if (reason instanceof DOMException && reason.name === 'AbortError') return
        setError(reason instanceof Error ? reason.message : String(reason))
      })
      .finally(() => setLoading(false))
    return () => controller.abort()
  }, [])

  useEffect(() => {
    if (!accounts.length) return
    if (!accounts.some((account) => account.source_id === sourceId)) {
      setSourceId(initialSource || accounts[0].source_id)
    }
  }, [accounts, initialSource, sourceId])

  useEffect(() => {
    if (newPosition) return
    if (availablePositions[0]) setNewPosition(availablePositions[0].strategy_name)
  }, [availablePositions, newPosition])

  useEffect(() => {
    if (newOrder) return
    if (orders[0]) setNewOrder(orders[0].strategy_name)
  }, [newOrder, orders])

  const loadStudio = useCallback(async (nextSourceId: string, signal?: AbortSignal) => {
    const next = await getAccountStudio(nextSourceId, signal)
    setStudio(next)
    setLeverage(String(next.leverage))
    setCapacity(next.capacity ?? null)
    return next
  }, [])

  useEffect(() => {
    if (!sourceId) {
      setStudio(null)
      return
    }
    const controller = new AbortController()
    loadStudio(sourceId, controller.signal).catch((reason: unknown) => {
      if (reason instanceof DOMException && reason.name === 'AbortError') return
      setError(reason instanceof Error ? reason.message : String(reason))
    })
    return () => controller.abort()
  }, [loadStudio, sourceId])

  useEffect(() => {
    if (!sourceId) return
    const timer = window.setInterval(() => {
      void getAccountLive(sourceId)
        .then(setCapacity)
        .catch(() => undefined)
    }, 2_000)
    return () => window.clearInterval(timer)
  }, [sourceId])

  async function bindExecution(
    positionStrategyName: string,
    orderStrategyName: string,
    shares: number,
  ) {
    await saveAccountBinding(
      sourceId,
      positionStrategyName,
      positionStrategyName,
      orderStrategyName,
      shares,
    )
    const next = await getAccountStudio(sourceId)
    setStudio(next)
    setCapacity(next.capacity ?? null)
    await reloadCatalog()
  }

  return (
    <ConfigShell
      section="bindings"
      title="策略启用"
      description="把已创建的仓位策略挂到本账户，并为每条策略选择一个执行算法。占比双击修改后统一保存，合计必须等于 100%。Exec 上的策略名与仓位策略名相同。"
      saving={saving}
      error={error ?? catalogError ?? writeError}
      notice={notice}
    >
      <Alert tone="warning" className="mb-2">
        <strong className="font-medium">逻辑说明：</strong>
        先在「仓位策略」里定义目标仓位 → 在「下单策略」里维护执行算法模板（如 default_order）→
        在这里把二者关联并发布。不需要单独填发布名，系统会用仓位策略名写入 Exec。
      </Alert>

      {loading || catalogLoading ? (
        <Card>
          <CardContent className="flex items-center justify-center gap-2 py-16 text-sm text-muted">
            <LoaderCircle size={18} className="animate-spin-slow" />
            正在加载
          </CardContent>
        </Card>
      ) : accounts.length === 0 ? (
        <Alert tone="warning">当前没有可配置的 Exec 账户。</Alert>
      ) : (
        <div className="space-y-6">
          <CapacityPanel
            capacity={capacity ?? studio?.capacity}
            toolbar={
              <LeverageToolbar
                account={
                  <Label>
                    账户
                    <Select
                      value={sourceId}
                      onChange={(event) => {
                        const next = event.target.value
                        setSourceId(next)
                        window.history.replaceState({}, '', routes.configBindings(next))
                      }}
                    >
                      {accounts.map((entry) => (
                        <option key={entry.source_id} value={entry.source_id}>
                          {entry.account} / {entry.source_id}
                        </option>
                      ))}
                    </Select>
                  </Label>
                }
                leverage={leverage}
                saving={saving}
                onLeverageChange={setLeverage}
                onSave={() =>
                  void withWrite(async () => {
                    const next = await saveAccountLeverage(sourceId, Number(leverage))
                    setStudio(next)
                    setCapacity(next.capacity ?? null)
                  })
                }
              />
            }
          />

          <Card>
            <CardHeader>
              <CardTitle className="flex items-center gap-2">
                <Plus size={16} /> 启用新策略
              </CardTitle>
              <CardDescription>选择一条尚未在本账户启用的仓位策略，并指定执行算法。</CardDescription>
            </CardHeader>
            <CardContent>
              {availablePositions.length === 0 ? (
                <p className="text-sm text-muted">
                  所有仓位策略都已启用。如需新增，请先在
                  <a href={routes.configPosition} className="mx-1 font-medium text-brand">
                    仓位策略
                  </a>
                  页创建。
                </p>
              ) : (
                <form
                  className="grid gap-4 md:grid-cols-[minmax(0,1.2fr)_minmax(0,1fr)_7.5rem_auto]"
                  onSubmit={(event) => {
                    event.preventDefault()
                    void withWrite(async () => {
                      await bindExecution(newPosition, newOrder, Number(newShares))
                      setNewPosition('')
                      setNewShares('1')
                    })
                  }}
                >
                  <Label>
                    仓位策略
                    <Select value={newPosition} onChange={(event) => setNewPosition(event.target.value)}>
                      {availablePositions.map((item) => (
                        <option key={item.strategy_name} value={item.strategy_name}>
                          {item.strategy_name}
                        </option>
                      ))}
                    </Select>
                    <FieldHint>即 Exec 上的 CTA 策略名，发布时原样使用。</FieldHint>
                  </Label>
                  <Label>
                    执行算法
                    <Select value={newOrder} onChange={(event) => setNewOrder(event.target.value)}>
                      {orders.map((item) => (
                        <option key={item.strategy_name} value={item.strategy_name}>
                          {item.strategy_name}
                        </option>
                      ))}
                    </Select>
                    <FieldHint>通常选 default_order，多条策略可共用。</FieldHint>
                  </Label>
                  <Label>
                    份数
                    <Input
                      value={newShares}
                      inputMode="decimal"
                      onChange={(event) => setNewShares(event.target.value)}
                    />
                    <FieldHint>初始份数；启用后再用下方占比统一调整</FieldHint>
                  </Label>
                  <Button type="submit" variant="primary" className="md:self-end" disabled={saving}>
                    <Plus size={15} /> 启用
                  </Button>
                </form>
              )}
            </CardContent>
          </Card>

          {(studio?.bindings ?? []).length > 0 ? (
            <AllocationEditor
              bindings={studio?.bindings ?? []}
              boundEquity={studio?.bound_equity_usdt ?? 0}
              saving={saving}
              onSave={(allocations) =>
                void withWrite(async () => {
                  const next = await saveAccountAllocations(sourceId, allocations)
                  setStudio(next)
                  setCapacity(next.capacity ?? null)
                })
              }
            />
          ) : null}

          <div className="space-y-4">
            <div className="flex items-center gap-2 text-sm font-medium text-ink">
              <Layers3 size={16} className="text-brand" />
              已启用策略
            </div>
            {(studio?.bindings ?? []).length === 0 ? (
              <Card>
                <CardContent className="py-12 text-center text-sm text-muted">
                  本账户还没有启用的策略。请在上方添加。
                </CardContent>
              </Card>
            ) : (
              (studio?.bindings ?? []).map((binding) => (
                <Card key={binding.binding_name}>
                  <CardContent className="space-y-4 pt-5">
                    <div className="flex flex-wrap items-start justify-between gap-3">
                      <div>
                        <p className="text-base font-semibold text-ink">{binding.position_strategy_name}</p>
                        <p className="mt-1 flex items-center gap-1.5 text-sm text-muted">
                          <SlidersHorizontal size={14} />
                          执行算法：{binding.order_strategy_name}
                        </p>
                      </div>
                      <span className="rounded-full bg-brand-soft px-3 py-1 text-sm font-semibold text-brand">
                        {percent(binding.allocation_ratio)}
                      </span>
                    </div>
                    <FieldHint>
                      {binding.shares} 份 × {money(binding.position_equity_usdt)} USDT ={' '}
                      {money(binding.shares * binding.position_equity_usdt)} USDT。占比在上方统一编辑，合计必须等于
                      100%。
                    </FieldHint>
                    <div className="flex flex-wrap items-end gap-3">
                      <Label className="min-w-[200px] flex-1">
                        更换执行算法
                        <Select
                          value={binding.order_strategy_name}
                          onChange={(event) =>
                            void withWrite(async () => {
                              await bindExecution(
                                binding.position_strategy_name,
                                event.target.value,
                                binding.shares,
                              )
                            })
                          }
                        >
                          {orders.map((item) => (
                            <option key={item.strategy_name} value={item.strategy_name}>
                              {item.strategy_name}
                            </option>
                          ))}
                        </Select>
                      </Label>
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
                          <Trash2 size={15} /> 停用
                        </Button>
                      </div>
                    </div>
                  </CardContent>
                </Card>
              ))
            )}
          </div>
        </div>
      )}
    </ConfigShell>
  )
}
