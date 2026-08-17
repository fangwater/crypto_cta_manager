import {
  CheckCircle2,
  LockKeyhole,
  RefreshCw,
  RotateCcw,
  Save,
  Settings,
  ShieldCheck,
  UnlockKeyhole,
} from 'lucide-react'
import { useCallback, useEffect, useMemo, useState } from 'react'
import {
  ApiError,
  authenticateOrderConfig,
  getDashboard,
  getOrderConfigStrategies,
  getOrderConfigStrategy,
  saveOrderParameters,
} from '../api'
import { AppNav } from '../components/AppNav'
import { timestampUs } from '../format'
import type {
  DashboardSnapshot,
  OrderParameters,
  OrderStrategyView,
} from '../types'

interface ParameterForm {
  single_order_usdt: string
  orders_per_batch: string
  maker_price_anchor: OrderParameters['maker_price_anchor']
  tick_spacing: string
  batch_interval_ms: string
  maker_timeout_ms: string
  max_maker_requotes: string
  target_tolerance_usdt: string
}

type NumericField = Exclude<keyof ParameterForm, 'maker_price_anchor'>

const fieldLabels: Record<keyof ParameterForm, string> = {
  single_order_usdt: '单笔名义金额',
  orders_per_batch: '每批订单数',
  maker_price_anchor: 'Maker 价格锚点',
  tick_spacing: 'Tick 间距',
  batch_interval_ms: '批次间隔',
  maker_timeout_ms: 'Maker 超时',
  max_maker_requotes: '最大重报价次数',
  target_tolerance_usdt: '目标容差',
}

function initialQuery(name: string) {
  return new URLSearchParams(window.location.search).get(name)?.trim() ?? ''
}

function toForm(parameters: OrderParameters): ParameterForm {
  return {
    single_order_usdt: String(parameters.single_order_usdt),
    orders_per_batch: String(parameters.orders_per_batch),
    maker_price_anchor: parameters.maker_price_anchor,
    tick_spacing: String(parameters.tick_spacing),
    batch_interval_ms: String(parameters.batch_interval_ms),
    maker_timeout_ms: String(parameters.maker_timeout_ms),
    max_maker_requotes: String(parameters.max_maker_requotes),
    target_tolerance_usdt: String(parameters.target_tolerance_usdt),
  }
}

function finiteNumber(value: string, label: string, minimum: number) {
  if (!value.trim()) throw new Error(`${label}不能为空`)
  const parsed = Number(value)
  if (!Number.isFinite(parsed) || parsed < minimum) {
    throw new Error(`${label}必须大于等于 ${minimum}`)
  }
  return parsed
}

function positiveNumber(value: string, label: string) {
  const parsed = finiteNumber(value, label, 0)
  if (parsed === 0) throw new Error(`${label}必须大于 0`)
  return parsed
}

function unsignedInteger(value: string, label: string, minimum: number) {
  const parsed = finiteNumber(value, label, minimum)
  if (!Number.isInteger(parsed) || parsed > 4_294_967_295) {
    throw new Error(`${label}必须是有效整数`)
  }
  return parsed
}

function parseForm(form: ParameterForm): OrderParameters {
  return {
    single_order_usdt: positiveNumber(form.single_order_usdt, fieldLabels.single_order_usdt),
    orders_per_batch: unsignedInteger(form.orders_per_batch, fieldLabels.orders_per_batch, 1),
    maker_price_anchor: form.maker_price_anchor,
    tick_spacing: unsignedInteger(form.tick_spacing, fieldLabels.tick_spacing, 0),
    batch_interval_ms: unsignedInteger(form.batch_interval_ms, fieldLabels.batch_interval_ms, 0),
    maker_timeout_ms: unsignedInteger(form.maker_timeout_ms, fieldLabels.maker_timeout_ms, 1),
    max_maker_requotes: unsignedInteger(
      form.max_maker_requotes,
      fieldLabels.max_maker_requotes,
      0,
    ),
    target_tolerance_usdt: finiteNumber(
      form.target_tolerance_usdt,
      fieldLabels.target_tolerance_usdt,
      0,
    ),
  }
}

function displayValue(field: keyof ParameterForm, value: string) {
  if (field !== 'maker_price_anchor') return value
  return value === 'own_best' ? '己方一档' : '对手一档 + 1 tick'
}

export function ConfigPage() {
  const [dashboard, setDashboard] = useState<DashboardSnapshot | null>(null)
  const [sourceId, setSourceId] = useState(() => initialQuery('source'))
  const [strategies, setStrategies] = useState<string[]>([])
  const [strategyName, setStrategyName] = useState(() => initialQuery('strategy'))
  const [loaded, setLoaded] = useState<OrderStrategyView | null>(null)
  const [form, setForm] = useState<ParameterForm | null>(null)
  const [token, setToken] = useState('')
  const [unlocked, setUnlocked] = useState(false)
  const [loading, setLoading] = useState(true)
  const [strategyLoading, setStrategyLoading] = useState(false)
  const [saving, setSaving] = useState(false)
  const [unlocking, setUnlocking] = useState(false)
  const [reloadRevision, setReloadRevision] = useState(0)
  const [pendingParameters, setPendingParameters] = useState<OrderParameters | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)

  const configurableAccounts = useMemo(
    () => (dashboard?.accounts ?? []).filter((account) => account.enabled && account.configurable),
    [dashboard],
  )
  const selectedAccount = configurableAccounts.find(
    (account) => account.source_id === sourceId,
  )
  const baselineForm = useMemo(
    () => (loaded ? toForm(loaded.order_parameters) : null),
    [loaded],
  )
  const dirty =
    form !== null && baselineForm !== null && JSON.stringify(form) !== JSON.stringify(baselineForm)

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
    if (!dashboard) return
    const accounts = (dashboard.accounts ?? []).filter(
      (account) => account.enabled && account.configurable,
    )
    if (!accounts.some((account) => account.source_id === sourceId)) {
      setSourceId(accounts[0]?.source_id ?? '')
    }
  }, [dashboard, sourceId])

  useEffect(() => {
    if (!sourceId) {
      setStrategies([])
      setStrategyName('')
      setLoaded(null)
      setForm(null)
      return
    }
    const controller = new AbortController()
    setStrategyLoading(true)
    setLoaded(null)
    setForm(null)
    getOrderConfigStrategies(sourceId, controller.signal)
      .then((response) => {
        setStrategies(response.strategies)
        setStrategyName((current) =>
          response.strategies.includes(current) ? current : (response.strategies[0] ?? ''),
        )
        setError(null)
      })
      .catch((reason: unknown) => {
        if (reason instanceof DOMException && reason.name === 'AbortError') return
        setStrategies([])
        setStrategyName('')
        setError(reason instanceof Error ? reason.message : String(reason))
      })
      .finally(() => {
        if (!controller.signal.aborted) setStrategyLoading(false)
      })
    return () => controller.abort()
  }, [sourceId])

  useEffect(() => {
    if (!sourceId || !strategyName) {
      setLoaded(null)
      setForm(null)
      return
    }
    const controller = new AbortController()
    setStrategyLoading(true)
    getOrderConfigStrategy(sourceId, strategyName, controller.signal)
      .then((strategy) => {
        setLoaded(strategy)
        setForm(toForm(strategy.order_parameters))
        setError(null)
        setNotice(null)
      })
      .catch((reason: unknown) => {
        if (reason instanceof DOMException && reason.name === 'AbortError') return
        setLoaded(null)
        setForm(null)
        setError(reason instanceof Error ? reason.message : String(reason))
      })
      .finally(() => {
        if (!controller.signal.aborted) setStrategyLoading(false)
      })
    return () => controller.abort()
  }, [reloadRevision, sourceId, strategyName])

  useEffect(() => {
    if (!dirty) return
    const warn = (event: BeforeUnloadEvent) => event.preventDefault()
    window.addEventListener('beforeunload', warn)
    return () => window.removeEventListener('beforeunload', warn)
  }, [dirty])

  const updateQuery = useCallback((nextSource: string, nextStrategy: string) => {
    const url = new URL(window.location.href)
    if (nextSource) url.searchParams.set('source', nextSource)
    else url.searchParams.delete('source')
    if (nextStrategy) url.searchParams.set('strategy', nextStrategy)
    else url.searchParams.delete('strategy')
    window.history.replaceState(null, '', `${url.pathname}${url.search}${url.hash}`)
  }, [])

  function confirmDiscard() {
    return !dirty || window.confirm('当前下单参数尚未保存，确认放弃修改？')
  }

  function selectSource(nextSource: string) {
    if (!confirmDiscard()) return
    setSourceId(nextSource)
    setStrategyName('')
    setNotice(null)
    updateQuery(nextSource, '')
  }

  function selectStrategy(nextStrategy: string) {
    if (!confirmDiscard()) return
    setStrategyName(nextStrategy)
    setNotice(null)
    updateQuery(sourceId, nextStrategy)
  }

  function setNumericField(field: NumericField, value: string) {
    setForm((current) => (current ? { ...current, [field]: value } : current))
    setNotice(null)
  }

  async function unlock(event: React.FormEvent) {
    event.preventDefault()
    const candidate = token.trim()
    if (!candidate) {
      setError('请输入写入密钥')
      return
    }
    setUnlocking(true)
    try {
      await authenticateOrderConfig(candidate)
      setToken(candidate)
      setUnlocked(true)
      setError(null)
      setNotice('写入权限已解锁')
    } catch (reason: unknown) {
      setUnlocked(false)
      setError(reason instanceof Error ? reason.message : String(reason))
    } finally {
      setUnlocking(false)
    }
  }

  function lock() {
    setUnlocked(false)
    setToken('')
    setPendingParameters(null)
    setNotice(null)
  }

  function prepareSave() {
    if (!form || !loaded || !unlocked) return
    if (loaded.updated_at_us === null) {
      setError('当前策略缺少配置版本，无法安全保存')
      return
    }
    try {
      setPendingParameters(parseForm(form))
      setError(null)
    } catch (reason: unknown) {
      setError(reason instanceof Error ? reason.message : String(reason))
    }
  }

  async function confirmSave() {
    if (!pendingParameters || !loaded || loaded.updated_at_us === null) return
    setSaving(true)
    try {
      const saved = await saveOrderParameters(
        sourceId,
        strategyName,
        loaded.updated_at_us,
        pendingParameters,
        token,
      )
      setLoaded(saved)
      setForm(toForm(saved.order_parameters))
      setPendingParameters(null)
      setError(null)
      setNotice('下单参数已保存')
    } catch (reason: unknown) {
      setPendingParameters(null)
      if (reason instanceof ApiError && reason.status === 401) lock()
      setError(
        reason instanceof ApiError && reason.status === 409
          ? '配置版本已变化，请重新加载后再修改'
          : reason instanceof Error
            ? reason.message
            : String(reason),
      )
    } finally {
      setSaving(false)
    }
  }

  const changes = useMemo(() => {
    if (!form || !baselineForm) return []
    return (Object.keys(fieldLabels) as (keyof ParameterForm)[])
      .filter((field) => form[field] !== baselineForm[field])
      .map((field) => ({
        field,
        before: displayValue(field, baselineForm[field]),
        after: displayValue(field, form[field]),
      }))
  }, [baselineForm, form])

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
              <p>下单配置</p>
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

        <section className="config-heading" aria-labelledby="config-title">
          <div className="section-heading">
            <div>
              <p className="eyebrow">ORDER EXECUTION</p>
              <h2 id="config-title">{selectedAccount?.account ?? '下单参数'}</h2>
            </div>
            {loaded && (
              <span className="workspace-updated">
                <CheckCircle2 size={14} />
                {timestampUs(loaded.updated_at_us)}
              </span>
            )}
          </div>
        </section>

        <section className="config-context" aria-label="配置范围">
          <label>
            <span>账户</span>
            <select
              value={sourceId}
              disabled={loading || configurableAccounts.length === 0}
              onChange={(event) => selectSource(event.target.value)}
            >
              {configurableAccounts.map((account) => (
                <option key={account.source_id} value={account.source_id}>
                  {account.account} · {account.venue}
                </option>
              ))}
            </select>
          </label>
          <label>
            <span>策略</span>
            <select
              value={strategyName}
              disabled={strategyLoading || strategies.length === 0}
              onChange={(event) => selectStrategy(event.target.value)}
            >
              {strategies.length === 0 && <option value="">暂无策略</option>}
              {strategies.map((strategy) => (
                <option key={strategy} value={strategy}>
                  {strategy}
                </option>
              ))}
            </select>
          </label>
          <button
            type="button"
            className="icon-button"
            title="重新加载"
            aria-label="重新加载配置"
            disabled={!strategyName || strategyLoading}
            onClick={() => {
              if (confirmDiscard()) setReloadRevision((current) => current + 1)
            }}
          >
            <RefreshCw size={17} className={strategyLoading ? 'is-spinning' : ''} />
          </button>
        </section>

        <section className="config-editor" aria-labelledby="parameters-title">
          <div className="panel-heading config-editor__heading">
            <div>
              <p className="eyebrow">BATCH EXEC</p>
              <h2 id="parameters-title">下单参数</h2>
            </div>
            <div className="config-readonly-state">
              <LockKeyhole size={14} />
              <span>仓位只读</span>
              <strong>
                {loaded ? `${loaded.nonzero_target_count} / ${loaded.target_count}` : '--'}
              </strong>
            </div>
          </div>

          {form && loaded ? (
            <>
              <fieldset className="parameter-grid" disabled={!unlocked || saving}>
                <NumberField
                  label={fieldLabels.single_order_usdt}
                  code="single_order_usdt"
                  unit="USDT"
                  value={form.single_order_usdt}
                  onChange={(value) => setNumericField('single_order_usdt', value)}
                />
                <NumberField
                  label={fieldLabels.orders_per_batch}
                  code="orders_per_batch"
                  value={form.orders_per_batch}
                  integer
                  onChange={(value) => setNumericField('orders_per_batch', value)}
                />
                <div className="parameter-field parameter-field--wide">
                  <div className="parameter-label">
                    <span>{fieldLabels.maker_price_anchor}</span>
                    <code>maker_price_anchor</code>
                  </div>
                  <div className="anchor-segmented" aria-label="Maker 价格锚点">
                    <button
                      type="button"
                      className={form.maker_price_anchor === 'own_best' ? 'is-active' : ''}
                      onClick={() =>
                        setForm((current) =>
                          current ? { ...current, maker_price_anchor: 'own_best' } : current,
                        )
                      }
                    >
                      己方一档
                    </button>
                    <button
                      type="button"
                      className={
                        form.maker_price_anchor === 'opposite_best_plus_one_tick'
                          ? 'is-active'
                          : ''
                      }
                      onClick={() =>
                        setForm((current) =>
                          current
                            ? {
                                ...current,
                                maker_price_anchor: 'opposite_best_plus_one_tick',
                              }
                            : current,
                        )
                      }
                    >
                      对手一档 + 1 tick
                    </button>
                  </div>
                </div>
                <NumberField
                  label={fieldLabels.tick_spacing}
                  code="tick_spacing"
                  unit="ticks"
                  value={form.tick_spacing}
                  integer
                  onChange={(value) => setNumericField('tick_spacing', value)}
                />
                <NumberField
                  label={fieldLabels.batch_interval_ms}
                  code="batch_interval_ms"
                  unit="ms"
                  value={form.batch_interval_ms}
                  integer
                  onChange={(value) => setNumericField('batch_interval_ms', value)}
                />
                <NumberField
                  label={fieldLabels.maker_timeout_ms}
                  code="maker_timeout_ms"
                  unit="ms"
                  value={form.maker_timeout_ms}
                  integer
                  onChange={(value) => setNumericField('maker_timeout_ms', value)}
                />
                <NumberField
                  label={fieldLabels.max_maker_requotes}
                  code="max_maker_requotes"
                  value={form.max_maker_requotes}
                  integer
                  onChange={(value) => setNumericField('max_maker_requotes', value)}
                />
                <NumberField
                  label={fieldLabels.target_tolerance_usdt}
                  code="target_tolerance_usdt"
                  unit="USDT"
                  value={form.target_tolerance_usdt}
                  onChange={(value) => setNumericField('target_tolerance_usdt', value)}
                />
              </fieldset>

              <footer className="config-editor__footer">
                {unlocked ? (
                  <div className="config-unlocked">
                    <ShieldCheck size={17} />
                    <span>已解锁</span>
                    <button type="button" className="text-button" onClick={lock}>
                      锁定
                    </button>
                  </div>
                ) : (
                  <form className="config-auth" onSubmit={(event) => void unlock(event)}>
                    <LockKeyhole size={16} />
                    <input
                      type="password"
                      value={token}
                      autoComplete="off"
                      placeholder="写入密钥"
                      aria-label="写入密钥"
                      onChange={(event) => setToken(event.target.value)}
                    />
                    <button type="submit" disabled={unlocking || !token.trim()}>
                      <UnlockKeyhole size={15} />
                      解锁
                    </button>
                  </form>
                )}
                <div className="config-commands">
                  <button
                    type="button"
                    className="command-button command-button--secondary"
                    disabled={!dirty || saving}
                    onClick={() => baselineForm && setForm(baselineForm)}
                  >
                    <RotateCcw size={15} />
                    重置
                  </button>
                  <button
                    type="button"
                    className="command-button command-button--primary"
                    disabled={!dirty || !unlocked || saving}
                    onClick={prepareSave}
                  >
                    <Save size={15} />
                    保存
                  </button>
                </div>
              </footer>
            </>
          ) : (
            <div className="config-empty">
              {strategyLoading ? <RefreshCw className="is-spinning" size={19} /> : null}
              <span>{strategyLoading ? '加载中' : '暂无可配置策略'}</span>
            </div>
          )}
        </section>
      </main>

      {pendingParameters && (
        <div className="config-dialog-backdrop" role="presentation">
          <section className="config-dialog" role="dialog" aria-modal="true" aria-labelledby="confirm-title">
            <div className="config-dialog__head">
              <ShieldCheck size={19} />
              <div>
                <h2 id="confirm-title">确认保存下单参数</h2>
                <code>{strategyName}</code>
              </div>
            </div>
            <div className="config-change-list">
              {changes.map((change) => (
                <div key={change.field}>
                  <span>{fieldLabels[change.field]}</span>
                  <code>{change.before}</code>
                  <strong>→</strong>
                  <code>{change.after}</code>
                </div>
              ))}
            </div>
            <div className="config-dialog__actions">
              <button
                type="button"
                className="command-button command-button--secondary"
                disabled={saving}
                onClick={() => setPendingParameters(null)}
              >
                取消
              </button>
              <button
                type="button"
                className="command-button command-button--primary"
                disabled={saving}
                onClick={() => void confirmSave()}
              >
                <Save size={15} />
                {saving ? '保存中' : '确认保存'}
              </button>
            </div>
          </section>
        </div>
      )}
    </div>
  )
}

function NumberField({
  label,
  code,
  unit,
  value,
  integer = false,
  onChange,
}: {
  label: string
  code: string
  unit?: string
  value: string
  integer?: boolean
  onChange: (value: string) => void
}) {
  return (
    <label className="parameter-field">
      <span className="parameter-label">
        <span>{label}</span>
        <code>{code}</code>
      </span>
      <span className="parameter-input">
        <input
          type="number"
          inputMode={integer ? 'numeric' : 'decimal'}
          step={integer ? '1' : 'any'}
          value={value}
          onChange={(event) => onChange(event.target.value)}
        />
        {unit && <span>{unit}</span>}
      </span>
    </label>
  )
}
