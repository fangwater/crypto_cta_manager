import {
  Activity,
  CalendarRange,
  Check,
  Clock3,
  Coins,
  Database,
  LoaderCircle,
  RefreshCw,
  Search,
  Sigma,
  WalletCards,
} from 'lucide-react'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import './nav-dashboard.css'
import { getDashboard, getHealth, getTimeline } from './api'
import { AppShell } from './components/AppShell'
import { WorkspacePage } from './pages/WorkspacePage'
import { AccountOverviewPage } from './pages/AccountOverviewPage'
import { PositionStrategyPage } from './pages/config/PositionStrategyPage'
import { OrderStrategyPage } from './pages/config/OrderStrategyPage'
import { AccountBindingsPage } from './pages/config/AccountBindingsPage'
import { DocsPage } from './pages/DocsPage'
import { ExecutionCostPage } from './pages/ExecutionCostPage'
import { normalizePath, readSourceId, routes } from './lib/routes'
import {
  NavTimelineChart,
  navSeriesMeta,
} from './components/NavTimelineChart'
import {
  feeBps,
  integer,
  money,
  quantity,
  signedClass,
  strategyLabel,
  timestampUs,
} from './format'
import type {
  AggregateSymbolNavReport,
  DashboardSnapshot,
  FeeMode,
  HealthResponse,
  NavSeriesKey,
  NavTotals,
  SourceNavReport,
  TimelineChartMode,
  TimelineSnapshot,
} from './types'

const UI_POLL_MS = 15_000

const rangeOptions = [
  { key: 'ALL', days: null },
  { key: '1D', days: 1 },
  { key: '7D', days: 7 },
  { key: '30D', days: 30 },
] as const

type QuickRange = (typeof rangeOptions)[number]['key']

const seriesOptions: NavSeriesKey[] = [
  'nav_change_before_fee_quote',
  'nav_change_after_fee_quote',
  'realized_pnl_before_fee_quote',
  'floating_pnl_quote',
  'estimated_trading_fee_quote',
]

const ZERO_TOTALS: NavTotals = {
  fill_count: 0,
  volume_quote: 0,
  maker_fill_count: 0,
  maker_volume_quote: 0,
  taker_fill_count: 0,
  taker_volume_quote: 0,
  unknown_liquidity_fill_count: 0,
  unknown_liquidity_volume_quote: 0,
  realized_pnl_before_fee_quote: 0,
  estimated_trading_fee_quote: 0,
  realized_pnl_after_fee_quote: 0,
  floating_pnl_quote: 0,
  nav_change_before_fee_quote: 0,
  nav_change_after_fee_quote: 0,
}

function navValue(totals: NavTotals, feeMode: FeeMode) {
  return feeMode === 'after'
    ? totals.nav_change_after_fee_quote
    : totals.nav_change_before_fee_quote
}

function realizedValue(totals: NavTotals, feeMode: FeeMode) {
  return feeMode === 'after'
    ? totals.realized_pnl_after_fee_quote
    : totals.realized_pnl_before_fee_quote
}

function toDatetimeLocal(ms: number) {
  const date = new Date(ms)
  const local = new Date(ms - date.getTimezoneOffset() * 60_000)
  return local.toISOString().slice(0, 23)
}

function fromDatetimeLocal(value: string) {
  return new Date(value).getTime()
}

function sourceStartMs(source: SourceNavReport) {
  const timestamp =
    source.initial_position_snapshot_ts_us ?? source.first_fill_ts_us
  return timestamp === null ? null : Math.ceil(timestamp / 1_000)
}

function scopeStartMs(
  dashboard: DashboardSnapshot,
  scope: string,
  fallbackEndMs: number,
) {
  const sources =
    scope === 'all'
      ? dashboard.report.sources
      : dashboard.report.sources.filter((source) => source.source_id === scope)
  const starts = sources
    .map(sourceStartMs)
    .filter((value): value is number => value !== null && value <= fallbackEndMs)
  return starts.length === 0 ? fallbackEndMs : Math.min(...starts)
}

function initialScope() {
  return new URLSearchParams(window.location.search).get('source')?.trim() || 'all'
}

export default function App() {
  const path = normalizePath(window.location.pathname)
  if (path === '/' || path === '/manager/workspace') return <WorkspacePage />
  if (path === '/manager/account') return <AccountOverviewPage />
  if (path === '/manager/config') {
    const source = readSourceId()
    window.location.replace(source ? routes.configBindings(source) : routes.configPosition)
    return null
  }
  if (path === '/manager/config/position') return <PositionStrategyPage />
  if (path === '/manager/config/order') return <OrderStrategyPage />
  if (path === '/manager/config/bindings') return <AccountBindingsPage />
  if (path === '/manager/docs') return <DocsPage />
  if (path === '/manager/execution-cost') return <ExecutionCostPage />
  return <NavPage />
}

function NavPage() {
  const [dashboard, setDashboard] = useState<DashboardSnapshot | null>(null)
  const [health, setHealth] = useState<HealthResponse | null>(null)
  const [timeline, setTimeline] = useState<TimelineSnapshot | null>(null)
  const [scope, setScope] = useState(initialScope)
  const [feeMode, setFeeMode] = useState<FeeMode>('after')
  const [chartMode, setChartMode] =
    useState<TimelineChartMode>('portfolio')
  const [visibleSeries, setVisibleSeries] = useState<NavSeriesKey[]>([
    'nav_change_before_fee_quote',
    'nav_change_after_fee_quote',
    'floating_pnl_quote',
  ])
  const [selectedSymbols, setSelectedSymbols] = useState<string[] | null>(null)
  const [selectedStrategies, setSelectedStrategies] = useState<string[] | null>(null)
  const [query, setQuery] = useState('')
  const [startInput, setStartInput] = useState('')
  const [endInput, setEndInput] = useState('')
  const [startMs, setStartMs] = useState<number | null>(null)
  const [endMs, setEndMs] = useState<number | null>(null)
  const [activeRange, setActiveRange] = useState<QuickRange | null>('ALL')
  const [loading, setLoading] = useState(true)
  const [timelineLoading, setTimelineLoading] = useState(false)
  const [refreshing, setRefreshing] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [timelineError, setTimelineError] = useState<string | null>(null)
  const [timelineRevision, setTimelineRevision] = useState(0)
  const initialized = useRef(false)

  const refreshDashboard = useCallback(
    async (signal?: AbortSignal, manual = false) => {
      if (manual) setRefreshing(true)
      try {
        const [nextDashboard, nextHealth] = await Promise.all([
          getDashboard(signal),
          getHealth(signal),
        ])
        setDashboard(nextDashboard)
        setHealth(nextHealth)
        setError(null)
      } catch (reason: unknown) {
        if (reason instanceof DOMException && reason.name === 'AbortError') return
        setError(reason instanceof Error ? reason.message : String(reason))
      } finally {
        setLoading(false)
        if (manual) setRefreshing(false)
      }
    },
    [],
  )

  useEffect(() => {
    const controller = new AbortController()
    void refreshDashboard(controller.signal)
    const timer = window.setInterval(() => void refreshDashboard(), UI_POLL_MS)
    return () => {
      controller.abort()
      window.clearInterval(timer)
    }
  }, [refreshDashboard])

  useEffect(() => {
    if (!dashboard || initialized.current) return
    initialized.current = true
    const nextEnd = Date.now()
    const nextStart = scopeStartMs(dashboard, 'all', nextEnd)
    setStartInput(toDatetimeLocal(nextStart))
    setEndInput(toDatetimeLocal(nextEnd))
    setStartMs(nextStart)
    setEndMs(nextEnd)
  }, [dashboard])

  useEffect(() => {
    if (
      dashboard &&
      scope !== 'all' &&
      !dashboard.report.sources.some((source) => source.source_id === scope)
    ) {
      setScope('all')
      setSelectedSymbols(null)
    }
  }, [dashboard, scope])

  useEffect(() => {
    if (startMs === null || endMs === null) return
    if (selectedSymbols?.length === 0) {
      setTimelineLoading(false)
      setTimelineError(null)
      return
    }
    const controller = new AbortController()
    setTimelineLoading(true)
    setTimelineError(null)
    getTimeline({
      startMs,
      endMs,
      sourceIds: scope === 'all' ? undefined : [scope],
      symbols: selectedSymbols ?? undefined,
      maxPoints: 3_500,
      signal: controller.signal,
    })
      .then(setTimeline)
      .catch((reason: unknown) => {
        if (reason instanceof DOMException && reason.name === 'AbortError') return
        setTimelineError(reason instanceof Error ? reason.message : String(reason))
      })
      .finally(() => {
        if (!controller.signal.aborted) setTimelineLoading(false)
      })
    return () => controller.abort()
  }, [endMs, scope, selectedSymbols, startMs, timelineRevision])

  const selectedSource = useMemo(
    () => dashboard?.report.sources.find((source) => source.source_id === scope),
    [dashboard, scope],
  )
  const availableSymbols = timeline?.report.available_symbols ?? []
  const availableStrategies = timeline?.report.available_strategies ?? []
  const selectedSet = useMemo(
    () =>
      new Set(
        selectedSymbols ??
          timeline?.report.selected_symbols ??
          timeline?.report.available_symbols ??
          [],
      ),
    [selectedSymbols, timeline],
  )
  const visibleRows = useMemo(() => {
    const normalizedQuery = query.trim().toUpperCase()
    const rows = normalizedQuery
      ? (timeline?.report.symbols ?? []).filter((row) =>
          row.symbol.includes(normalizedQuery),
        )
      : [...(timeline?.report.symbols ?? [])]
    return rows.sort(
      (left, right) =>
        Math.abs(right.nav_change_after_fee_quote) -
          Math.abs(left.nav_change_after_fee_quote) ||
        left.symbol.localeCompare(right.symbol),
    )
  }, [query, timeline])
  const selectedStrategySet = useMemo(
    () => new Set(selectedStrategies ?? availableStrategies),
    [availableStrategies, selectedStrategies],
  )
  const visibleStrategyPoints = useMemo(
    () =>
      (timeline?.report.strategy_points ?? [])
        .filter((strategy) => selectedStrategySet.has(strategy.strategy))
        .sort(
          (left, right) =>
            Math.abs(right.summary.nav_change_after_fee_quote) -
              Math.abs(left.summary.nav_change_after_fee_quote) ||
            left.strategy.localeCompare(right.strategy),
        ),
    [selectedStrategySet, timeline],
  )
  const noSymbolsSelected = selectedSymbols?.length === 0
  const noStrategiesSelected = selectedStrategies?.length === 0
  const totals = noSymbolsSelected
    ? ZERO_TOTALS
    : (timeline?.report.summary ?? null)
  const effectiveStartMs = dashboard
    ? scopeStartMs(dashboard, scope, endMs ?? Date.now())
    : 0

  function applyRange() {
    if (!dashboard) return
    const nextStart = fromDatetimeLocal(startInput)
    const nextEnd = fromDatetimeLocal(endInput)
    const earliest = scopeStartMs(dashboard, scope, nextEnd)
    if (
      !Number.isFinite(nextStart) ||
      !Number.isFinite(nextEnd) ||
      nextStart < earliest ||
      nextEnd < nextStart
    ) {
      setTimelineError('时间范围无效')
      return
    }
    setActiveRange(null)
    setStartMs(nextStart)
    setEndMs(nextEnd)
  }

  function applyQuickRange(
    key: QuickRange,
    nextScope = scope,
    endOverride?: number,
  ) {
    if (!dashboard) return
    const parsedEnd = fromDatetimeLocal(endInput)
    const nextEnd = endOverride ?? (Number.isFinite(parsedEnd) ? parsedEnd : Date.now())
    const earliest = scopeStartMs(dashboard, nextScope, nextEnd)
    const days = rangeOptions.find((option) => option.key === key)?.days ?? null
    const nextStart =
      days === null
        ? earliest
        : Math.max(earliest, nextEnd - days * 86_400_000)
    setActiveRange(key)
    setStartInput(toDatetimeLocal(nextStart))
    setEndInput(toDatetimeLocal(nextEnd))
    setStartMs(nextStart)
    setEndMs(nextEnd)
  }

  function selectScope(nextScope: string) {
    setScope(nextScope)
    const url = new URL(window.location.href)
    if (nextScope === 'all') url.searchParams.delete('source')
    else url.searchParams.set('source', nextScope)
    window.history.replaceState(null, '', `${url.pathname}${url.search}${url.hash}`)
    setSelectedSymbols(null)
    setSelectedStrategies(null)
    setTimeline(null)
    if (!dashboard || endMs === null) return
    if (activeRange) {
      applyQuickRange(activeRange, nextScope)
      return
    }
    const earliest = scopeStartMs(dashboard, nextScope, endMs)
    if (startMs === null || startMs < earliest) {
      setStartInput(toDatetimeLocal(earliest))
      setStartMs(earliest)
    }
  }

  function toggleSymbol(symbol: string) {
    if (selectedSymbols === null) {
      setSelectedSymbols(availableSymbols.filter((item) => item !== symbol))
      return
    }
    const next = selectedSymbols.includes(symbol)
      ? selectedSymbols.filter((item) => item !== symbol)
      : [...selectedSymbols, symbol].sort()
    setSelectedSymbols(next.length === availableSymbols.length ? null : next)
  }

  function toggleSeries(key: NavSeriesKey) {
    setVisibleSeries((current) => {
      if (current.includes(key)) {
        return current.length === 1
          ? current
          : current.filter((item) => item !== key)
      }
      return [...current, key]
    })
  }

  function toggleStrategy(strategy: string) {
    if (selectedStrategies === null) {
      setSelectedStrategies(availableStrategies.filter((item) => item !== strategy))
      return
    }
    const next = selectedStrategies.includes(strategy)
      ? selectedStrategies.filter((item) => item !== strategy)
      : [...selectedStrategies, strategy].sort()
    setSelectedStrategies(next.length === availableStrategies.length ? null : next)
  }

  async function manualRefresh() {
    await refreshDashboard(undefined, true)
    if (activeRange) applyQuickRange(activeRange, scope, Date.now())
    else setTimelineRevision((current) => current + 1)
  }

  if (loading && !dashboard) return <LoadingScreen />

  return (
    <AppShell
      active="manager"
      title="CTA NAV"
      subtitle="CTA 组合净值"
      icon={Activity}
      className="max-w-[1240px] px-5 sm:px-6"
      actions={
        <div className="flex items-center gap-3 rounded-xl border border-border-soft bg-canvas/80 px-3 py-2">
          <span
            className={`h-2 w-2 shrink-0 rounded-full ${
              health?.status === 'ok' ? 'bg-emerald-500' : 'bg-amber-500'
            }`}
          />
          <div className="hidden min-w-0 sm:block">
            <p className="text-xs font-medium text-ink">
              {health?.status === 'ok' ? '运行正常' : '数据延迟'}
            </p>
            <time className="text-[11px] text-subtle">
              {timestampUs(
                timeline?.generated_at_us ?? dashboard?.generated_at_us ?? null,
              )}
            </time>
          </div>
          <button
            type="button"
            className="grid h-9 w-9 place-items-center rounded-lg border border-border bg-surface text-muted transition-colors hover:border-brand-ring hover:text-brand disabled:opacity-50"
            title="刷新数据"
            aria-label="刷新数据"
            disabled={refreshing}
            onClick={() => void manualRefresh()}
          >
            <RefreshCw size={16} className={refreshing ? 'animate-spin-slow' : ''} />
          </button>
        </div>
      }
    >
        {error && <div className="error-banner">数据请求失败：{error}</div>}
        {timelineError && (
          <div className="error-banner">净值重算失败：{timelineError}</div>
        )}
        {health?.last_refresh_error && (
          <div className="warning-banner">
            最近一次服务端重算失败，当前显示上一份有效数据
          </div>
        )}

        <section className="pnl-toolbar" aria-label="净值查询范围">
          <div className="date-range">
            <CalendarRange size={18} />
            <label>
              <span>开始</span>
              <input
                type="datetime-local"
                step="0.001"
                value={startInput}
                min={effectiveStartMs ? toDatetimeLocal(effectiveStartMs) : undefined}
                max={endInput}
                onChange={(event) => setStartInput(event.target.value)}
              />
            </label>
            <span className="range-separator">至</span>
            <label>
              <span>结束</span>
              <input
                type="datetime-local"
                step="0.001"
                value={endInput}
                min={startInput}
                onChange={(event) => setEndInput(event.target.value)}
              />
            </label>
            <button className="refresh-button" type="button" onClick={applyRange}>
              <RefreshCw size={15} />
              查询
            </button>
          </div>
          <div className="segmented segmented--compact" aria-label="快捷时间范围">
            {rangeOptions.map((option) => (
              <button
                key={option.key}
                type="button"
                className={activeRange === option.key ? 'is-active' : ''}
                onClick={() => applyQuickRange(option.key)}
              >
                {option.key}
              </button>
            ))}
          </div>
        </section>

        <section className="overview" aria-labelledby="overview-title">
          <div className="section-heading">
            <div>
              <p className="eyebrow">PORTFOLIO</p>
              <h2 id="overview-title">
                {selectedSource?.account ?? '综合账户'}
              </h2>
            </div>
            <div className="control-row">
              <div className="segmented" aria-label="账户范围">
                <button
                  type="button"
                  className={scope === 'all' ? 'is-active' : ''}
                  onClick={() => selectScope('all')}
                >
                  综合
                </button>
                {dashboard?.report.sources.map((source) => (
                  <button
                    type="button"
                    className={scope === source.source_id ? 'is-active' : ''}
                    key={source.source_id}
                    onClick={() => selectScope(source.source_id)}
                  >
                    {source.account}
                  </button>
                ))}
              </div>
              <div className="segmented segmented--compact" aria-label="费率口径">
                <button
                  type="button"
                  className={feeMode === 'after' ? 'is-active' : ''}
                  onClick={() => setFeeMode('after')}
                >
                  费后
                </button>
                <button
                  type="button"
                  className={feeMode === 'before' ? 'is-active' : ''}
                  onClick={() => setFeeMode('before')}
                >
                  费前
                </button>
              </div>
            </div>
          </div>

          <div className="summary-strip">
            <SummaryItem
              icon={<Sigma size={18} />}
              label="区间净值变化"
              value={totals ? navValue(totals, feeMode) : null}
              signed
            />
            <SummaryItem
              icon={<WalletCards size={18} />}
              label="区间已实现"
              value={totals ? realizedValue(totals, feeMode) : null}
              signed
            />
            <SummaryItem
              icon={<Activity size={18} />}
              label="区间浮动盈亏"
              value={totals?.floating_pnl_quote ?? null}
              signed
            />
            <SummaryItem
              icon={<Coins size={18} />}
              label="区间估算手续费"
              value={totals?.estimated_trading_fee_quote ?? null}
            />
            <SummaryItem
              icon={<Database size={18} />}
              label="区间成交额"
              value={totals?.volume_quote ?? null}
            />
          </div>
        </section>

        <section className="chart-section" aria-labelledby="chart-title">
          <div className="panel-heading pnl-chart-header">
            <div>
              <p className="eyebrow">NAV TIMELINE</p>
              <h2 id="chart-title">收益曲线</h2>
            </div>
            <div className="chart-controls">
              <div className="segmented segmented--compact" aria-label="曲线视图">
                <button
                  type="button"
                  className={chartMode === 'portfolio' ? 'is-active' : ''}
                  onClick={() => setChartMode('portfolio')}
                >
                  组合
                </button>
                <button
                  type="button"
                  className={chartMode === 'symbols' ? 'is-active' : ''}
                  onClick={() => setChartMode('symbols')}
                >
                  分币
                </button>
                <button
                  type="button"
                  className={chartMode === 'strategies' ? 'is-active' : ''}
                  onClick={() => setChartMode('strategies')}
                >
                  分策略
                </button>
              </div>
              {chartMode === 'symbols' && (
                <span className="symbol-series-count">
                  {selectedSet.size} 条币种曲线
                </span>
              )}
              {chartMode === 'strategies' && (
                <span className="symbol-series-count">
                  {selectedStrategySet.size} 条策略曲线
                </span>
              )}
            </div>
          </div>
          <div className="chart-body has-picker">
            <div className="chart-stage">
              {timeline &&
                !noSymbolsSelected &&
                !(chartMode === 'strategies' && noStrategiesSelected) && (
                <NavTimelineChart
                  points={timeline.report.points}
                  symbolPoints={timeline.report.symbol_points}
                  strategyPoints={visibleStrategyPoints}
                  visibleSeries={visibleSeries}
                  mode={chartMode}
                  feeMode={feeMode}
                />
              )}
              {noSymbolsSelected && (
                <div className="chart-empty">当前未选择币种</div>
              )}
              {!noSymbolsSelected &&
                chartMode === 'strategies' &&
                noStrategiesSelected && (
                  <div className="chart-empty">当前未选择策略</div>
                )}
              {!timeline && !timelineLoading && (
                <div className="chart-empty">暂无净值时间线</div>
              )}
              {timelineLoading && (
                <div className="chart-loading">
                  <LoaderCircle size={20} />
                  <span>计算中</span>
                </div>
              )}
            </div>
            {chartMode === 'portfolio' ? (
              <aside className="symbol-curve-picker" aria-label="净值曲线选择">
                <div className="symbol-curve-picker__header">
                  <strong>PNL</strong>
                </div>
                <div className="symbol-curve-picker__list">
                  {seriesOptions.map((key) => (
                    <label key={key}>
                      <input
                        type="checkbox"
                        checked={visibleSeries.includes(key)}
                        onChange={() => toggleSeries(key)}
                      />
                      <span
                        className="series-swatch"
                        style={{ backgroundColor: navSeriesMeta[key].color }}
                      />
                      <span>{navSeriesMeta[key].label}</span>
                    </label>
                  ))}
                </div>
              </aside>
            ) : chartMode === 'symbols' ? (
              <aside className="symbol-curve-picker" aria-label="分币曲线选择">
                <div className="symbol-curve-picker__header">
                  <strong>币种</strong>
                  <div className="symbol-curve-picker__actions">
                    <button
                      type="button"
                      onClick={() => setSelectedSymbols(null)}
                      disabled={selectedSymbols === null}
                    >
                      全选
                    </button>
                    <button
                      type="button"
                      onClick={() => setSelectedSymbols([])}
                      disabled={noSymbolsSelected}
                    >
                      全不选
                    </button>
                  </div>
                </div>
                <div className="symbol-curve-picker__list">
                  {availableSymbols.map((symbol) => (
                    <label key={symbol}>
                      <input
                        type="checkbox"
                        checked={selectedSet.has(symbol)}
                        onChange={() => toggleSymbol(symbol)}
                      />
                      <span>{symbol}</span>
                    </label>
                  ))}
                </div>
              </aside>
            ) : (
              <aside className="symbol-curve-picker" aria-label="分策略曲线选择">
                <div className="symbol-curve-picker__header">
                  <strong>策略</strong>
                  <div className="symbol-curve-picker__actions">
                    <button
                      type="button"
                      onClick={() => setSelectedStrategies(null)}
                      disabled={selectedStrategies === null}
                    >
                      全选
                    </button>
                    <button
                      type="button"
                      onClick={() => setSelectedStrategies([])}
                      disabled={noStrategiesSelected}
                    >
                      全不选
                    </button>
                  </div>
                </div>
                <div className="symbol-curve-picker__list">
                  {availableStrategies.map((strategy) => (
                    <label key={strategy} title={strategy}>
                      <input
                        type="checkbox"
                        checked={selectedStrategySet.has(strategy)}
                        onChange={() => toggleStrategy(strategy)}
                      />
                      <span>{strategyLabel(strategy)}</span>
                    </label>
                  ))}
                </div>
              </aside>
            )}
          </div>
          {timeline && (
            <div className="chart-foot">
              <span>
                <Database size={13} />
                {integer(noSymbolsSelected ? 0 : timeline.report.summary.fill_count)} fills
              </span>
              <span>
                {integer(
                  noSymbolsSelected
                    ? 0
                    : chartMode === 'portfolio'
                      ? timeline.report.points.length
                      : chartMode === 'symbols'
                        ? timeline.report.symbol_points.reduce(
                            (count, item) => count + item.points.length,
                            0,
                          )
                        : visibleStrategyPoints.reduce(
                            (count, item) => count + item.points.length,
                            0,
                          ),
                )}{' '}
                15min ticks
              </span>
              <span>
                {chartMode === 'strategies'
                  ? `${selectedStrategySet.size} strategies`
                  : `${noSymbolsSelected ? 0 : selectedSet.size} symbols`}
              </span>
              <span>{timeline.generation_duration_ms} ms</span>
              {timeline.report.sampled && <span>sampled</span>}
            </div>
          )}
        </section>

        <section className="strategy-section" aria-labelledby="strategies-title">
          <div className="panel-heading">
            <div>
              <p className="eyebrow">STRATEGY ATTRIBUTION</p>
              <h2 id="strategies-title">策略盈亏</h2>
            </div>
            <span className="generation-time">
              {visibleStrategyPoints.length} / {availableStrategies.length} strategies
            </span>
          </div>
          <div className="table-wrap">
            <table className="strategy-table">
              <thead>
                <tr>
                  <th>策略</th>
                  <th className="numeric">成交</th>
                  <th className="numeric">持仓币种</th>
                  <th className="numeric">期末总敞口</th>
                  <th className="numeric">区间已实现</th>
                  <th className="numeric">区间浮动盈亏</th>
                  <th className="numeric">区间手续费</th>
                  <th className="numeric">区间净值变化</th>
                </tr>
              </thead>
              <tbody>
                {visibleStrategyPoints.map((strategy) => (
                  <tr key={strategy.strategy}>
                    <td className="strategy-cell" title={strategy.strategy}>
                      <strong>{strategyLabel(strategy.strategy)}</strong>
                      {strategyLabel(strategy.strategy) !== strategy.strategy && (
                        <code>{strategy.strategy}</code>
                      )}
                    </td>
                    <td className="numeric mono">
                      {integer(strategy.summary.fill_count)}
                    </td>
                    <td className="numeric mono">{integer(strategy.symbol_count)}</td>
                    <td className="numeric mono">
                      {money(strategy.gross_position_value_quote)}
                    </td>
                    <td
                      className={`numeric mono ${signedClass(
                        realizedValue(strategy.summary, feeMode),
                      )}`}
                    >
                      {money(realizedValue(strategy.summary, feeMode))}
                    </td>
                    <td
                      className={`numeric mono ${signedClass(
                        strategy.summary.floating_pnl_quote,
                      )}`}
                    >
                      {money(strategy.summary.floating_pnl_quote)}
                    </td>
                    <td className="numeric mono">
                      {money(strategy.summary.estimated_trading_fee_quote)}
                    </td>
                    <td
                      className={`numeric mono nav-cell ${signedClass(
                        navValue(strategy.summary, feeMode),
                      )}`}
                    >
                      {money(navValue(strategy.summary, feeMode))}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
            {visibleStrategyPoints.length === 0 && (
              <div className="empty-state">没有选中的策略</div>
            )}
          </div>
        </section>

        <section className="positions-section" aria-labelledby="positions-title">
          <div className="panel-heading panel-heading--table">
            <div>
              <p className="eyebrow">WINDOW / END STATE</p>
              <h2 id="positions-title">区间盈亏与期末仓位</h2>
            </div>
            <label className="search-field">
              <Search size={15} aria-hidden="true" />
              <input
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                placeholder="搜索币种"
                aria-label="搜索币种"
              />
            </label>
          </div>
          <div className="table-wrap">
            <table>
              <thead>
                <tr>
                  <th>币种</th>
                  <th className="numeric">期末净数量</th>
                  <th className="numeric">期末净敞口</th>
                  <th className="numeric">区间已实现</th>
                  <th className="numeric">区间浮动盈亏</th>
                  <th className="numeric">区间手续费</th>
                  <th className="numeric">区间净值变化</th>
                </tr>
              </thead>
              <tbody>
                {visibleRows.map((row) => (
                  <SymbolTableRow
                    key={row.symbol}
                    row={row}
                    feeMode={feeMode}
                    selected={selectedSet.has(row.symbol)}
                    onToggle={() => toggleSymbol(row.symbol)}
                  />
                ))}
              </tbody>
            </table>
            {visibleRows.length === 0 && (
              <div className="empty-state">没有匹配的币种</div>
            )}
          </div>
        </section>

        <section className="sources-section" aria-labelledby="sources-title">
          <div className="panel-heading">
            <div>
              <p className="eyebrow">SOURCES</p>
              <h2 id="sources-title">账户数据源</h2>
            </div>
            <span className="generation-time">
              缓存重算 {dashboard?.generation_duration_ms ?? 0} ms
            </span>
          </div>
          <div className="source-list">
            {dashboard?.report.sources.map((source) => (
              <div className="source-row" key={source.source_id}>
                <div className="source-identity">
                  <span className="venue-mark">B</span>
                  <div>
                    <strong>{source.account}</strong>
                    <code>{source.source_id}</code>
                  </div>
                </div>
                <SourceDatum label="场所" value={source.configured_venue} />
                <SourceDatum
                  label="初始快照"
                  value={timestampUs(source.initial_position_snapshot_ts_us)}
                />
                <SourceDatum label="累计成交" value={integer(source.fill_count)} />
                <SourceDatum
                  label="Maker / Taker 费率"
                  value={`${feeBps(source.maker_fee_rate)} / ${feeBps(source.taker_fee_rate)}`}
                />
                <SourceDatum
                  label="累计费后净值"
                  value={`${money(source.nav_change_after_fee_quote)} USDT`}
                  tone={signedClass(source.nav_change_after_fee_quote)}
                />
              </div>
            ))}
          </div>
        </section>
    </AppShell>
  )
}

function SummaryItem({
  icon,
  label,
  value,
  signed = false,
}: {
  icon: React.ReactNode
  label: string
  value: number | null
  signed?: boolean
}) {
  return (
    <div className="summary-item">
      {icon}
      <div>
        <span>{label}</span>
        <strong className={signed && value !== null ? signedClass(value) : ''}>
          {value === null ? '--' : money(value)}{' '}
          {value !== null && <small>USDT</small>}
        </strong>
      </div>
    </div>
  )
}

function SymbolTableRow({
  row,
  feeMode,
  selected,
  onToggle,
}: {
  row: AggregateSymbolNavReport
  feeMode: FeeMode
  selected: boolean
  onToggle: () => void
}) {
  return (
    <tr className={selected ? 'is-selected' : ''}>
      <td>
        <button
          className={`symbol-check ${selected ? 'is-checked' : ''}`}
          type="button"
          onClick={onToggle}
          title={selected ? '移出组合' : '加入组合'}
          aria-label={`${selected ? '移出' : '加入'} ${row.symbol}`}
        >
          {selected && <Check size={13} />}
        </button>
        <strong className="symbol-name">{row.symbol.replace(/USDT$/, '')}</strong>
        <span className="symbol-quote">/USDT</span>
      </td>
      <td className={`numeric mono ${signedClass(row.net_quantity)}`}>
        {quantity(row.net_quantity)}
      </td>
      <td className={`numeric mono ${signedClass(row.net_position_value_quote)}`}>
        {money(row.net_position_value_quote)}
      </td>
      <td className={`numeric mono ${signedClass(realizedValue(row, feeMode))}`}>
        {money(realizedValue(row, feeMode))}
      </td>
      <td className={`numeric mono ${signedClass(row.floating_pnl_quote)}`}>
        {money(row.floating_pnl_quote)}
      </td>
      <td className="numeric mono">{money(row.estimated_trading_fee_quote)}</td>
      <td className={`numeric mono nav-cell ${signedClass(navValue(row, feeMode))}`}>
        {money(navValue(row, feeMode))}
      </td>
    </tr>
  )
}

function SourceDatum({
  label,
  value,
  tone = '',
}: {
  label: string
  value: string
  tone?: string
}) {
  return (
    <div className="source-datum">
      <span>{label}</span>
      <strong className={tone}>{value}</strong>
    </div>
  )
}

function LoadingScreen() {
  return (
    <div className="grid min-h-screen place-items-center bg-canvas px-6">
      <div className="w-full max-w-sm rounded-2xl border border-border bg-surface p-8 shadow-card">
        <div className="flex items-center gap-3 text-brand">
          <Activity size={22} strokeWidth={2.2} />
          <strong className="text-lg font-semibold text-ink">CTA NAV</strong>
        </div>
        <div className="mt-6 space-y-3">
          <div className="h-2 animate-pulse rounded-full bg-border-soft" />
          <div className="h-2 w-2/3 animate-pulse rounded-full bg-border-soft" />
        </div>
        <p className="mt-6 flex items-center gap-2 text-sm text-muted">
          <Clock3 size={16} />
          正在加载净值数据…
        </p>
      </div>
    </div>
  )
}
