import {
  ChartNoAxesCombined,
  CircleAlert,
  LoaderCircle,
  RefreshCw,
} from 'lucide-react'
import { useCallback, useEffect, useMemo, useState } from 'react'
import { getDashboard, getPositionHistory } from '../api'
import { AppShell, PageIntro, StatTile } from '../components/AppShell'
import { PositionHistoryChart } from '../components/PositionHistoryChart'
import { Alert, Badge } from '../components/ui/Badge'
import { Button } from '../components/ui/Button'
import { Card, CardContent, CardHeader, CardTitle } from '../components/ui/Card'
import { Input, Label, Select } from '../components/ui/Field'
import { money, quantity } from '../format'
import { cn } from '../lib/cn'
import { readSourceId } from '../lib/routes'
import type {
  DashboardSnapshot,
  CurrentEquityAvailability,
  PositionHistorySnapshot,
  PositionHistorySymbolPoint,
} from '../types'

const DAY_MS = 86_400_000
const rangeOptions = [
  { key: 'ALL', days: null },
  { key: '1D', days: 1 },
  { key: '3D', days: 3 },
  { key: '7D', days: 7 },
  { key: '30D', days: 30 },
] as const

type QuickRange = (typeof rangeOptions)[number]['key']

function toDatetimeLocal(ms: number) {
  const date = new Date(ms)
  const local = new Date(ms - date.getTimezoneOffset() * 60_000)
  return local.toISOString().slice(0, 16)
}

function fromDatetimeLocal(value: string) {
  const parsed = new Date(value).getTime()
  return Number.isFinite(parsed) ? parsed : null
}

function sourceLabel(dashboard: DashboardSnapshot | null, sourceId: string) {
  const account = dashboard?.accounts?.find((item) => item.source_id === sourceId)
  return account ? `${account.account} (${sourceId})` : sourceId
}

function availabilityLabel(value: PositionHistorySymbolPoint['availability']) {
  const labels: Record<PositionHistorySymbolPoint['availability'], string> = {
    ok: '正常',
    missing_mark: '缺最后成交价',
    missing_anchor: '缺仓位起点',
    missing_equity: '缺权益',
    stale_equity: '权益过期',
    missing_notional: '缺名义仓位',
    stale_notional: '名义仓位过期',
    missing_position: '缺仓位',
    incomplete: '未完整',
    missing_sample: '重建数据缺失',
    nonpositive_equity: '权益非正',
  }
  return labels[value]
}

function currentEquityAvailabilityLabel(value: CurrentEquityAvailability) {
  const labels: Record<CurrentEquityAvailability, string> = {
    ok: '正常',
    missing: '缺当前权益',
    stale: '当前权益过期',
    nonfinite: '当前权益无效',
    nonpositive: '当前权益非正',
    incomplete: '账户权益不完整',
  }
  return labels[value]
}

function valuationSourceLabel(value: PositionHistorySymbolPoint['valuation_source']) {
  switch (value) {
    case 'last_fill':
      return '最后成交价'
    case 'initial_reference':
      return '初始参考价'
    case 'unavailable':
      return '不可用'
  }
}

export function PositionHistoryPage() {
  const now = Date.now()
  const initialSource = readSourceId()
  const [dashboard, setDashboard] = useState<DashboardSnapshot | null>(null)
  const [snapshot, setSnapshot] = useState<PositionHistorySnapshot | null>(null)
  const [scope, setScope] = useState(initialSource || 'all')
  const [startInput, setStartInput] = useState(toDatetimeLocal(now - 3 * DAY_MS))
  const [endInput, setEndInput] = useState(toDatetimeLocal(now))
  const [range, setRange] = useState<QuickRange | null>('3D')
  const [requestRange, setRequestRange] = useState({ startMs: now - 3 * DAY_MS, endMs: now })
  const [selectedSymbols, setSelectedSymbols] = useState<string[] | null>(null)
  const [loading, setLoading] = useState(false)
  const [dashboardError, setDashboardError] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    const controller = new AbortController()
    void getDashboard(controller.signal)
      .then((next) => {
        setDashboard(next)
        setDashboardError(null)
      })
      .catch((reason: unknown) => {
        if (reason instanceof DOMException && reason.name === 'AbortError') return
        setDashboardError(reason instanceof Error ? reason.message : String(reason))
      })
    return () => controller.abort()
  }, [])

  useEffect(() => {
    const controller = new AbortController()
    setLoading(true)
    setError(null)
    setSnapshot(null)
    void getPositionHistory({
      startMs: requestRange.startMs,
      endMs: requestRange.endMs,
      sourceIds: scope === 'all' ? undefined : [scope],
      maxPoints: 1_000,
      signal: controller.signal,
    })
      .then((next) => {
        setSnapshot(next)
        setSelectedSymbols((current) => {
          if (current === null) return null
          return current.filter((symbol) => next.available_symbols.includes(symbol))
        })
      })
      .catch((reason: unknown) => {
        if (reason instanceof DOMException && reason.name === 'AbortError') return
        setError(reason instanceof Error ? reason.message : String(reason))
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false)
      })
    return () => controller.abort()
  }, [requestRange, scope])

  useEffect(() => {
    if (!dashboard || scope === 'all') return
    if (!dashboard.accounts?.some((account) => account.source_id === scope)) {
      setScope('all')
    }
  }, [dashboard, scope])

  const availableSymbols = snapshot?.available_symbols ?? []
  const selectedSymbolSet = useMemo(
    () => new Set(selectedSymbols ?? availableSymbols),
    [availableSymbols, selectedSymbols],
  )
  const quantitySeries = useMemo(() => {
    const selected = new Map<string, PositionHistorySymbolPoint[]>()
    for (const point of snapshot?.symbol_points ?? []) {
      if (!selectedSymbolSet.has(point.symbol)) continue
      const key = `${point.source_id}\u0000${point.symbol}\u0000${point.venue}`
      const values = selected.get(key) ?? []
      values.push(point)
      selected.set(key, values)
    }
    return [...selected.entries()]
      .map(([key, points]) => {
        const [sourceId, symbol, venue] = key.split('\u0000')
        const compactName = scope === 'all' ? `${sourceId} · ${symbol}` : symbol
        return { name: `${compactName} · ${venue}`, points: points.sort((a, b) => a.ts_ms - b.ts_ms) }
      })
      .sort((left, right) => left.name.localeCompare(right.name))
  }, [scope, selectedSymbolSet, snapshot])
  const latestRows = useMemo(() => {
    const rows = new Map<string, PositionHistorySymbolPoint>()
    for (const point of snapshot?.symbol_points ?? []) {
      if (!selectedSymbolSet.has(point.symbol)) continue
      const key = `${point.source_id}\u0000${point.symbol}\u0000${point.venue}`
      const current = rows.get(key)
      if (!current || point.ts_ms > current.ts_ms) rows.set(key, point)
    }
    return [...rows.values()].sort(
      (left, right) =>
        left.source_id.localeCompare(right.source_id) || left.symbol.localeCompare(right.symbol),
    )
  }, [selectedSymbolSet, snapshot])
  const latestPortfolio = snapshot?.portfolio_points.at(-1)
  const currentEquity = snapshot?.current_equity
  const incompleteCount = snapshot?.portfolio_points.filter((point) => point.availability !== 'ok').length ?? 0
  const hasSymbolNotional = quantitySeries.some((series) =>
    series.points.some((point) => point.gross_notional_usdt !== null),
  )
  const hasLeverageContribution = quantitySeries.some((series) =>
    series.points.some((point) => point.leverage_contribution !== null),
  )

  const query = useCallback(() => {
    const startMs = fromDatetimeLocal(startInput)
    const endMs = fromDatetimeLocal(endInput)
    if (startMs === null || endMs === null || startMs > endMs) {
      setError('时间范围无效')
      return
    }
    setRange(null)
    setRequestRange({ startMs, endMs })
  }, [endInput, startInput])

  function applyRange(nextRange: QuickRange) {
    const endMs = Date.now()
    const days = rangeOptions.find((entry) => entry.key === nextRange)?.days ?? null
    const availableStart = snapshot?.available_sources
      .map((source) => source.first_ts_ms)
      .filter((value): value is number => Number.isFinite(value))
      .reduce((earliest, value) => Math.min(earliest, value), Number.POSITIVE_INFINITY)
    const startMs =
      days === null
        ? Number.isFinite(availableStart) && availableStart !== undefined
          ? Math.min(endMs, availableStart)
          : endMs - DAY_MS
        : endMs - days * DAY_MS
    setRange(nextRange)
    setStartInput(toDatetimeLocal(startMs))
    setEndInput(toDatetimeLocal(endMs))
    setRequestRange({ startMs, endMs })
  }

  function selectScope(nextScope: string) {
    setScope(nextScope)
    setSelectedSymbols(null)
    const url = new URL(window.location.href)
    if (nextScope === 'all') url.searchParams.delete('source')
    else url.searchParams.set('source', nextScope)
    window.history.replaceState(null, '', `${url.pathname}${url.search}${url.hash}`)
  }

  function toggleSymbol(symbol: string) {
    setSelectedSymbols((current) => {
      const next = new Set(current ?? availableSymbols)
      if (next.has(symbol)) next.delete(symbol)
      else next.add(symbol)
      return [...next].sort()
    })
  }

  return (
    <AppShell
      active="positions"
      title="历史仓位"
      subtitle="由初始仓位与订单成交历史重建，使用最后成交价估值"
      icon={ChartNoAxesCombined}
      actions={
        <Button type="button" size="sm" variant="primary" disabled={loading} onClick={query}>
          {loading ? <LoaderCircle size={15} className="animate-spin-slow" /> : <RefreshCw size={15} />}
          查询
        </Button>
      }
    >
      <PageIntro eyebrow="Position history" title="历史仓位" />

      {dashboardError && <Alert className="mb-4">账户列表失败：{dashboardError}</Alert>}
      {error && <Alert className="mb-4">历史仓位读取失败：{error}</Alert>}

      <Card className="mb-6">
        <CardHeader>
          <CardTitle>查询条件</CardTitle>
        </CardHeader>
        <CardContent className="grid gap-4 md:grid-cols-2 xl:grid-cols-4">
          <Label>
            开始
            <Input type="datetime-local" value={startInput} onChange={(event) => setStartInput(event.target.value)} />
          </Label>
          <Label>
            结束
            <Input type="datetime-local" value={endInput} onChange={(event) => setEndInput(event.target.value)} />
          </Label>
          <Label>
            账户
            <Select value={scope} onChange={(event) => selectScope(event.target.value)}>
              <option value="all">全部账户</option>
              {(dashboard?.accounts ?? []).map((account) => (
                <option key={account.source_id} value={account.source_id}>
                  {sourceLabel(dashboard, account.source_id)}
                </option>
              ))}
            </Select>
          </Label>
          <div className="grid content-end gap-1.5">
            <span className="text-xs font-medium text-muted">快捷范围</span>
            <div className="grid grid-cols-5 overflow-hidden rounded-lg border border-border">
              {rangeOptions.map((option) => (
                <button
                  key={option.key}
                  type="button"
                  className={cn(
                    'h-9 border-r border-border text-xs font-medium last:border-r-0',
                    range === option.key ? 'bg-brand text-white' : 'bg-surface text-muted hover:bg-canvas',
                  )}
                  onClick={() => applyRange(option.key)}
                >
                  {option.key}
                </button>
              ))}
            </div>
          </div>
        </CardContent>
      </Card>

      <div className="mb-6 grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
        <StatTile
          label="当前账户权益"
          value={currentEquity?.equity_usdt == null ? '--' : `${money(currentEquity.equity_usdt)} USDT`}
          hint={currentEquity ? `${currentEquityAvailabilityLabel(currentEquity.availability)} · 本次查询时读取` : undefined}
        />
        <StatTile
          label="最近总名义金额"
          value={latestPortfolio?.gross_notional_usdt == null ? '--' : `${money(latestPortfolio.gross_notional_usdt)} USDT`}
          hint="优先使用最后成交价；无历史成交时使用初始参考价"
        />
        <StatTile
          label="按当前权益计算的杠杆率"
          value={latestPortfolio?.gross_leverage == null ? '--' : `${latestPortfolio.gross_leverage.toFixed(3)}x`}
          hint="所选名义金额 / 当前账户权益"
        />
        <StatTile
          label="重建时间点"
          value={snapshot ? String(snapshot.portfolio_points.length) : '--'}
          hint="成交事件与初始仓位快照"
        />
      </div>

      <Card className="mb-6 overflow-hidden">
        <CardHeader className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
          <div>
            <CardTitle>所选总名义金额</CardTitle>
            <p className="mt-1 text-xs text-muted">优先按最后成交价计算；无历史成交时使用初始参考价。币种筛选不影响当前账户权益分母。</p>
          </div>
          <Badge tone="neutral">{snapshot?.selected_source_ids.map((source) => sourceLabel(dashboard, source)).join(' · ') || '加载中'}</Badge>
        </CardHeader>
        <CardContent className="p-0">
          {snapshot && snapshot.portfolio_points.length > 0 ? (
            <PositionHistoryChart kind="notional" points={snapshot.portfolio_points} />
          ) : (
            <ChartEmpty loading={loading} />
          )}
        </CardContent>
      </Card>

      <Card className="mb-6 overflow-hidden">
        <CardHeader>
            <CardTitle>分币杠杆贡献（按当前权益）</CardTitle>
        </CardHeader>
        <CardContent className="p-0">
          {hasLeverageContribution ? (
            <PositionHistoryChart kind="symbol-leverage" series={quantitySeries} />
          ) : (
            <ChartEmpty loading={loading} message="当前重建结果未提供分币杠杆贡献" />
          )}
        </CardContent>
      </Card>

      <Card className="mb-6 overflow-hidden">
        <CardHeader className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
          <div>
            <CardTitle>按当前权益计算的杠杆率</CardTitle>
            <p className="mt-1 text-xs text-muted">整段历史仓位均使用本次查询取得的当前账户权益作为分母，不展示或推算历史权益。当前权益不可用、过期或非正时，数量和名义金额仍显示，杠杆率留空。</p>
          </div>
          {incompleteCount > 0 && <Badge tone="warning">{incompleteCount} 个不可计算点</Badge>}
        </CardHeader>
        <CardContent className="p-0">
          {snapshot && snapshot.portfolio_points.length > 0 ? (
            <PositionHistoryChart kind="leverage" points={snapshot.portfolio_points} />
          ) : (
            <ChartEmpty loading={loading} />
          )}
        </CardContent>
      </Card>

      <Card className="mb-6 overflow-hidden">
        <CardHeader>
          <CardTitle>当前账户权益分母</CardTitle>
          <p className="mt-1 text-xs text-muted">这些是杠杆率计算使用的当前权益观测，不是历史权益曲线。</p>
        </CardHeader>
        <div className="overflow-x-auto">
          <table className="w-full min-w-[640px] text-left text-sm">
            <thead className="border-b border-border-soft bg-canvas/50 text-xs font-medium text-muted">
              <tr>
                <th className="px-5 py-3">账户</th>
                <th className="px-5 py-3 text-right">当前权益</th>
                <th className="px-5 py-3">观测时间</th>
                <th className="px-5 py-3">状态</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-border-soft">
              {(currentEquity?.accounts ?? []).map((account) => (
                <tr key={account.source_id}>
                  <td className="px-5 py-3 text-ink">{sourceLabel(dashboard, account.source_id)}</td>
                  <td className="px-5 py-3 text-right font-mono text-ink">{account.equity_usdt == null ? '--' : `${money(account.equity_usdt)} USDT`}</td>
                  <td className="px-5 py-3 font-mono text-xs text-muted">{account.ts_ms == null ? '--' : new Date(account.ts_ms).toLocaleString()}</td>
                  <td className="px-5 py-3"><CurrentEquityAvailability value={account.availability} /></td>
                </tr>
              ))}
              {(currentEquity?.accounts.length ?? 0) === 0 && (
                <tr><td className="px-5 py-8 text-center text-sm text-muted" colSpan={4}>未取得当前账户权益</td></tr>
              )}
            </tbody>
          </table>
        </div>
      </Card>

      <Card className="mb-6 overflow-hidden">
        <CardHeader className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
          <div>
            <CardTitle>分币名义金额</CardTitle>
            <p className="mt-1 text-xs text-muted">按账户、币种和场馆保留，优先使用最后成交价估值。</p>
          </div>
          <span className="text-xs text-subtle">{selectedSymbolSet.size} 币种</span>
        </CardHeader>
        <div className="flex flex-wrap items-center gap-x-4 gap-y-2 border-b border-border-soft px-5 py-3 text-xs">
          <button type="button" className="font-medium text-brand" onClick={() => setSelectedSymbols(null)}>全部</button>
          <button type="button" className="font-medium text-brand" onClick={() => setSelectedSymbols([])}>全不选</button>
          {availableSymbols.map((symbol) => (
            <label key={symbol} className="inline-flex cursor-pointer items-center gap-1.5 text-muted">
              <input type="checkbox" checked={selectedSymbolSet.has(symbol)} onChange={() => toggleSymbol(symbol)} />
              {symbol}
            </label>
          ))}
        </div>
        <CardContent className="p-0">
          {hasSymbolNotional ? (
            <PositionHistoryChart kind="symbol-notional" series={quantitySeries} />
          ) : (
            <ChartEmpty loading={loading} message="当前重建结果未提供分币名义金额" />
          )}
        </CardContent>
      </Card>

      <Card className="mb-6 overflow-hidden">
        <CardHeader>
            <CardTitle>分币历史数量</CardTitle>
        </CardHeader>
        <CardContent className="p-0">
          {quantitySeries.length > 0 ? (
            <PositionHistoryChart kind="quantity" series={quantitySeries} />
          ) : (
            <ChartEmpty loading={loading} message="当前未选择币种或无分币历史" />
          )}
        </CardContent>
      </Card>

      <Card className="overflow-hidden">
        <CardHeader className="flex items-center justify-between">
          <div>
            <CardTitle>最新分币仓位</CardTitle>
            <p className="mt-1 text-xs text-muted">各账户的最后一条重建记录。</p>
          </div>
          <Badge tone="neutral">{latestRows.length} 条</Badge>
        </CardHeader>
        <div className="overflow-x-auto">
          <table className="w-full min-w-[860px] text-left text-sm">
            <thead className="border-b border-border-soft bg-canvas/50 text-xs font-medium text-muted">
              <tr>
                <th className="px-5 py-3">账户</th>
                <th className="px-5 py-3">币种</th>
                <th className="px-5 py-3">场馆</th>
                <th className="px-5 py-3 text-right">数量</th>
                <th className="px-5 py-3 text-right">估值价格</th>
                <th className="px-5 py-3">价格来源</th>
                <th className="px-5 py-3 text-right">名义金额</th>
                <th className="px-5 py-3 text-right">杠杆贡献（当前权益）</th>
                <th className="px-5 py-3">重建时间</th>
                <th className="px-5 py-3">状态</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-border-soft">
              {latestRows.map((row) => (
                <tr key={`${row.source_id}-${row.symbol}-${row.venue}`}>
                  <td className="px-5 py-3 text-ink">{sourceLabel(dashboard, row.source_id)}</td>
                  <td className="px-5 py-3 font-medium text-ink">{row.symbol}</td>
                  <td className="px-5 py-3 text-muted">{row.venue}</td>
                  <td className="px-5 py-3 text-right font-mono text-ink">{row.quantity == null ? '--' : quantity(row.quantity)}</td>
                  <td className="px-5 py-3 text-right font-mono text-ink">{row.valuation_price == null ? '--' : money(row.valuation_price)}</td>
                  <td className="px-5 py-3 text-muted">{valuationSourceLabel(row.valuation_source)}</td>
                  <td className="px-5 py-3 text-right font-mono text-ink">{row.gross_notional_usdt == null ? '--' : `${money(row.gross_notional_usdt)} USDT`}</td>
                  <td className="px-5 py-3 text-right font-mono text-ink">{row.leverage_contribution == null ? '--' : `${row.leverage_contribution.toFixed(4)}x`}</td>
                  <td className="px-5 py-3 font-mono text-xs text-muted">{new Date(row.ts_ms).toLocaleString()}</td>
                  <td className="px-5 py-3"><Availability value={row.availability} /></td>
                </tr>
              ))}
              {latestRows.length === 0 && (
                <tr><td className="px-5 py-12 text-center text-sm text-muted" colSpan={10}>暂无分币历史仓位</td></tr>
              )}
            </tbody>
          </table>
        </div>
      </Card>
    </AppShell>
  )
}

function ChartEmpty({ loading, message = '暂无历史仓位数据' }: { loading: boolean; message?: string }) {
  return (
    <div className="grid h-[360px] place-items-center text-sm text-muted">
      <span className="flex items-center gap-2">
        {loading && <LoaderCircle size={16} className="animate-spin-slow" />}
        {message}
      </span>
    </div>
  )
}

function Availability({ value }: { value: PositionHistorySymbolPoint['availability'] }) {
  return value === 'ok' ? (
    <Badge tone="success">正常</Badge>
  ) : (
    <span className="inline-flex items-center gap-1 text-xs text-amber-700"><CircleAlert size={13} />{availabilityLabel(value)}</span>
  )
}

function CurrentEquityAvailability({ value }: { value: CurrentEquityAvailability }) {
  return value === 'ok' ? (
    <Badge tone="success">正常</Badge>
  ) : (
    <span className="inline-flex items-center gap-1 text-xs text-amber-700"><CircleAlert size={13} />{currentEquityAvailabilityLabel(value)}</span>
  )
}
