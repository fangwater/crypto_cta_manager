import {
  CheckCircle2,
  LockKeyhole,
  Plus,
  RefreshCw,
  Save,
  Settings,
  Trash2,
  UnlockKeyhole,
} from 'lucide-react'
import { useCallback, useEffect, useMemo, useState } from 'react'
import {
  ApiError,
  authenticateOrderConfig,
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
import { AppNav } from '../components/AppNav'
import { money } from '../format'
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

function emptyPosition(): PositionStrategy {
  return {
    strategy_name: '',
    equity_usdt: 10_000,
    targets: {},
    updated_at_us: 0,
  }
}

function emptyOrder(): CatalogOrderStrategy {
  return {
    strategy_name: '',
    order_parameters: { ...DEFAULT_ORDER },
    updated_at_us: 0,
  }
}

function percent(ratio: number) {
  return `${(ratio * 100).toFixed(1)}%`
}

function nextAllocationRatio(
  studio: AccountStudio,
  bindingName: string,
  nextEquity: number,
) {
  const replaced =
    studio.bindings.find((binding) => binding.binding_name === bindingName)?.position_equity_usdt ??
    0
  const total = studio.bound_equity_usdt - replaced + nextEquity
  return total > 0 ? nextEquity / total : 0
}

function parseTargets(raw: string): Record<string, number> {
  const parsed = JSON.parse(raw || '{}') as unknown
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
    throw new Error('targets 必须是 JSON 对象')
  }
  const targets: Record<string, number> = {}
  for (const [symbol, quantity] of Object.entries(parsed)) {
    const name = symbol.trim().toUpperCase()
    const value = Number(quantity)
    if (!name) throw new Error('品种名不能为空')
    if (!Number.isFinite(value)) throw new Error(`${name} 数量无效`)
    targets[name] = value
  }
  return targets
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
  const [targetsText, setTargetsText] = useState('{}')
  const [leverage, setLeverage] = useState('1')
  const [bindingName, setBindingName] = useState('')
  const [bindPosition, setBindPosition] = useState('')
  const [bindOrder, setBindOrder] = useState('')
  const [token, setToken] = useState('')
  const [unlocked, setUnlocked] = useState(false)
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
    if (!unlocked) {
      setError('请先解锁写权限')
      return
    }
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

  async function unlock() {
    setSaving(true)
    try {
      await authenticateOrderConfig(token)
      setUnlocked(true)
      setError(null)
      setNotice('写权限已解锁')
    } catch (reason: unknown) {
      setUnlocked(false)
      setError(reason instanceof ApiError ? reason.message : String(reason))
    } finally {
      setSaving(false)
    }
  }

  return (
    <div className="app-frame">
      <header className="app-header">
        <div className="app-header__inner">
          <div className="brand">
            <span className="brand__mark" aria-hidden="true">
              <Settings size={19} strokeWidth={2.1} />
            </span>
            <div>
              <h1>CTA Manager</h1>
              <p>策略组合配置</p>
            </div>
          </div>
          <div className="header-actions">
            <AppNav active="config" />
          </div>
        </div>
      </header>

      <main className="page-shell config-shell">
        {error && <div className="error-banner">{error}</div>}
        {notice && <div className="success-banner">{notice}</div>}

        <section className="config-heading">
          <div className="section-heading">
            <div>
              <p className="eyebrow">STRATEGY STUDIO</p>
              <h2>仓位策略、下单策略、账户绑定</h2>
            </div>
            <div className="config-auth">
              {unlocked ? <UnlockKeyhole size={16} /> : <LockKeyhole size={16} />}
              <input
                type="password"
                value={token}
                placeholder="写权限 token"
                onChange={(event) => {
                  setToken(event.target.value)
                  setUnlocked(false)
                }}
              />
              <button type="button" className="command-button" disabled={saving} onClick={() => void unlock()}>
                {unlocked ? '已解锁' : '解锁'}
              </button>
            </div>
          </div>
          <div className="studio-tabs">
            {(
              [
                ['position', '仓位策略'],
                ['order', '下单策略'],
                ['account', '账户组合'],
              ] as const
            ).map(([id, label]) => (
              <button
                key={id}
                type="button"
                className={tab === id ? 'is-active' : ''}
                onClick={() => setTab(id)}
              >
                {label}
              </button>
            ))}
          </div>
        </section>

        {loading ? (
          <div className="config-empty">正在加载配置</div>
        ) : tab === 'position' ? (
          <div className="studio-split">
            <aside>
              <button
                type="button"
                className="command-button"
                onClick={() => {
                  setSelectedPosition(emptyPosition())
                  setTargetsText('{}')
                }}
              >
                <Plus size={15} /> 新建仓位策略
              </button>
              <ul>
                {positions.map((item) => (
                  <li key={item.strategy_name}>
                    <button
                      type="button"
                      className={
                        selectedPosition.strategy_name === item.strategy_name ? 'is-active' : ''
                      }
                      onClick={() => {
                        setSelectedPosition(item)
                        setTargetsText(JSON.stringify(item.targets, null, 2))
                      }}
                    >
                      <strong>{item.strategy_name}</strong>
                      <span>权益 {money(item.equity_usdt)} USDT</span>
                    </button>
                  </li>
                ))}
              </ul>
            </aside>
            <form
              className="studio-form"
              onSubmit={(event) => {
                event.preventDefault()
                void withWrite(async () => {
                  const saved = await savePositionStrategy(
                    {
                      ...selectedPosition,
                      equity_usdt: Number(selectedPosition.equity_usdt),
                      targets: parseTargets(targetsText),
                    },
                    token,
                  )
                  setSelectedPosition(saved)
                  setTargetsText(JSON.stringify(saved.targets, null, 2))
                  await reloadCatalog()
                })
              }}
            >
              <label>
                策略名
                <input
                  value={selectedPosition.strategy_name}
                  onChange={(event) =>
                    setSelectedPosition({ ...selectedPosition, strategy_name: event.target.value })
                  }
                />
              </label>
              <label>
                权益金额 USDT
                <input
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
              </label>
              <p className="studio-hint">
                target 按这份参考权益定义，默认 10000 USDT。账户绑定后只用来算组合比例，不占用账户容量。
              </p>
              <label>
                目标仓位 JSON
                <textarea
                  rows={12}
                  value={targetsText}
                  onChange={(event) => setTargetsText(event.target.value)}
                />
              </label>
              <div className="studio-actions">
                <button type="submit" className="command-button command-button--primary" disabled={saving}>
                  <Save size={15} /> 保存仓位策略
                </button>
                {selectedPosition.strategy_name && (
                  <button
                    type="button"
                    className="command-button"
                    disabled={saving}
                    onClick={() =>
                      void withWrite(async () => {
                        await deletePositionStrategy(selectedPosition.strategy_name, token)
                        setSelectedPosition(emptyPosition())
                        setTargetsText('{}')
                        await reloadCatalog()
                      })
                    }
                  >
                    <Trash2 size={15} /> 删除
                  </button>
                )}
              </div>
            </form>
          </div>
        ) : tab === 'order' ? (
          <div className="studio-split">
            <aside>
              <button
                type="button"
                className="command-button"
                onClick={() => setSelectedOrder(emptyOrder())}
              >
                <Plus size={15} /> 新建下单策略
              </button>
              <ul>
                {orders.map((item) => (
                  <li key={item.strategy_name}>
                    <button
                      type="button"
                      className={selectedOrder.strategy_name === item.strategy_name ? 'is-active' : ''}
                      onClick={() => setSelectedOrder(item)}
                    >
                      <strong>{item.strategy_name}</strong>
                      <span>单笔 {item.order_parameters.single_order_usdt} USDT</span>
                    </button>
                  </li>
                ))}
              </ul>
            </aside>
            <form
              className="studio-form"
              onSubmit={(event) => {
                event.preventDefault()
                void withWrite(async () => {
                  const saved = await saveOrderStrategy(selectedOrder, token)
                  setSelectedOrder(saved)
                  await reloadCatalog()
                })
              }}
            >
              <label>
                策略名
                <input
                  value={selectedOrder.strategy_name}
                  onChange={(event) =>
                    setSelectedOrder({ ...selectedOrder, strategy_name: event.target.value })
                  }
                />
              </label>
              {(
                [
                  ['single_order_usdt', '单笔名义金额'],
                  ['orders_per_batch', '每批订单数'],
                  ['tick_spacing', 'Tick 间距'],
                  ['batch_interval_ms', '批次间隔'],
                  ['maker_timeout_ms', 'Maker 超时'],
                  ['max_maker_requotes', '最大重报价'],
                  ['target_tolerance_usdt', '目标容差'],
                ] as const
              ).map(([field, label]) => (
                <label key={field}>
                  {label}
                  <input
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
                </label>
              ))}
              <label>
                Maker 价格锚点
                <select
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
                </select>
              </label>
              <div className="studio-actions">
                <button type="submit" className="command-button command-button--primary" disabled={saving}>
                  <Save size={15} /> 保存下单策略
                </button>
                {selectedOrder.strategy_name && (
                  <button
                    type="button"
                    className="command-button"
                    disabled={saving}
                    onClick={() =>
                      void withWrite(async () => {
                        await deleteOrderStrategy(selectedOrder.strategy_name, token)
                        setSelectedOrder(emptyOrder())
                        await reloadCatalog()
                      })
                    }
                  >
                    <Trash2 size={15} /> 删除
                  </button>
                )}
              </div>
            </form>
          </div>
        ) : (
          <div className="studio-account">
            <div className="studio-form">
              <label>
                账户
                <select value={sourceId} onChange={(event) => setSourceId(event.target.value)}>
                  {accounts.map((account) => (
                    <option key={account.source_id} value={account.source_id}>
                      {account.account} / {account.source_id}
                    </option>
                  ))}
                </select>
              </label>
              <label>
                杠杆率
                <input value={leverage} onChange={(event) => setLeverage(event.target.value)} />
              </label>
              <p className="studio-hint">
                账户权益是实时变动的，这里不填金额、也不卡容量。绑定后按各仓位策略的参考权益算比例。
              </p>
              <button
                type="button"
                className="command-button command-button--primary"
                disabled={saving}
                onClick={() =>
                  void withWrite(async () => {
                    const next = await saveAccountStudio(sourceId, Number(leverage), token)
                    setStudio(next)
                  })
                }
              >
                <Save size={15} /> 保存杠杆
              </button>
              {studio && (
                <div className="studio-capacity">
                  <span>杠杆 {studio.leverage}</span>
                  <span>参考权益合计 {money(studio.bound_equity_usdt)}</span>
                </div>
              )}
            </div>
            <form
              className="studio-form"
              onSubmit={(event) => {
                event.preventDefault()
                void withWrite(async () => {
                  const next = await saveAccountBinding(
                    sourceId,
                    bindingName,
                    bindPosition,
                    bindOrder,
                    token,
                  )
                  setStudio(next)
                })
              }}
            >
              <h3>绑定组合</h3>
              <label>
                发布名
                <input
                  value={bindingName}
                  placeholder="写入 Exec 的策略名"
                  onChange={(event) => setBindingName(event.target.value)}
                />
              </label>
              <label>
                仓位策略
                <select value={bindPosition} onChange={(event) => setBindPosition(event.target.value)}>
                  {positions.map((item) => (
                    <option key={item.strategy_name} value={item.strategy_name}>
                      {item.strategy_name} / {money(item.equity_usdt)}
                    </option>
                  ))}
                </select>
              </label>
              <label>
                下单策略
                <select value={bindOrder} onChange={(event) => setBindOrder(event.target.value)}>
                  {orders.map((item) => (
                    <option key={item.strategy_name} value={item.strategy_name}>
                      {item.strategy_name}
                    </option>
                  ))}
                </select>
              </label>
              {studio && bindPosition && (
                <p className="studio-hint">
                  绑定后约占组合{' '}
                  {percent(
                    nextAllocationRatio(
                      studio,
                      bindingName,
                      positions.find((item) => item.strategy_name === bindPosition)?.equity_usdt ?? 0,
                    ),
                  )}
                  ，与账户实时权益无关
                </p>
              )}
              <button type="submit" className="command-button command-button--primary" disabled={saving}>
                <Plus size={15} /> 绑定到账户
              </button>
            </form>
            <div className="studio-bindings">
              {(studio?.bindings ?? []).map((binding) => (
                <article key={binding.binding_name}>
                  <h3>{binding.binding_name}</h3>
                  <p>
                    仓位 {binding.position_strategy_name} × 下单 {binding.order_strategy_name}
                  </p>
                  <p>
                    参考权益 {money(binding.position_equity_usdt)} USDT · 比例{' '}
                    {percent(binding.allocation_ratio)}
                  </p>
                  <div className="studio-actions">
                    <button
                      type="button"
                      className="command-button command-button--primary"
                      disabled={saving}
                      onClick={() =>
                        void withWrite(async () => {
                          await publishAccountBinding(sourceId, binding.binding_name, token)
                        })
                      }
                    >
                      <CheckCircle2 size={15} /> 发布到 Exec
                    </button>
                    <button
                      type="button"
                      className="command-button"
                      disabled={saving}
                      onClick={() =>
                        void withWrite(async () => {
                          await deleteAccountBinding(sourceId, binding.binding_name, token)
                          const next = await getAccountStudio(sourceId)
                          setStudio(next)
                        })
                      }
                    >
                      <Trash2 size={15} /> 解绑
                    </button>
                  </div>
                </article>
              ))}
            </div>
          </div>
        )}
        {saving && (
          <p className="studio-hint">
            <RefreshCw size={14} className="is-spinning" /> 正在写入
          </p>
        )}
      </main>
    </div>
  )
}
