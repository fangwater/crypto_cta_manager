import { ChevronLeft, ChevronRight, LoaderCircle, RefreshCw, Scale } from 'lucide-react'
import { useCallback, useEffect, useMemo, useState } from 'react'
import { getDashboard, getExecutionCost } from '../api'
import { AppShell, PageIntro, StatTile } from '../components/AppShell'
import { ExecutionCostChart } from '../components/ExecutionCostChart'
import { Alert, Badge } from '../components/ui/Badge'
import { Button } from '../components/ui/Button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '../components/ui/Card'
import { Input, Label, Select } from '../components/ui/Field'
import { money, quantity, timestampUs } from '../format'
import { cn } from '../lib/cn'
import { readSourceId } from '../lib/routes'
import type {
  DashboardSnapshot,
  ExecutionCostSnapshot,
  PositionUpdateExecutionCost,
  SymbolExecutionCost,
} from '../types'

const WINDOW_OPTIONS = [
  { label: '5 分钟', value: 300 },
  { label: '1 分钟', value: 60 },
  { label: '15 分钟', value: 900 },
  { label: '30 分钟', value: 1_800 },
] as const
const PAGE_SIZE = 25

function toDatetimeLocal(ms: number) {
  const date = new Date(ms)
  const local = new Date(ms - date.getTimezoneOffset() * 60_000)
  return local.toISOString().slice(0, 16)
}

function fromDatetimeLocal(value: string) {
  const parsed = new Date(value).getTime()
  return Number.isFinite(parsed) ? parsed : null
}

function optionalMoney(value: number | null | undefined) {
  return value == null ? '--' : money(value)
}

function moneyU(value: number) {
  return `${money(value)} U`
}

function costClass(value: number | null | undefined) {
  if (value == null) return ''
  if (value > 0) return 'number-negative'
  if (value < 0) return 'number-positive'
  return ''
}

function optionalBps(value: number | null | undefined) {
  return value == null ? '--' : `${value.toFixed(2)} bps`
}

function sideLabel(side: SymbolExecutionCost['side']) {
  if (side === 'buy') return '买'
  if (side === 'sell') return '卖'
  return '--'
}

export function ExecutionCostPage() {
  const initialSource = readSourceId()
  const [dashboard, setDashboard] = useState<DashboardSnapshot | null>(null)
  const [snapshot, setSnapshot] = useState<ExecutionCostSnapshot | null>(null)
  const [scope, setScope] = useState(initialSource || 'all')
  const [strategyName, setStrategyName] = useState('')
  const [windowSec, setWindowSec] = useState(300)
  const now = Date.now()
  const [startInput, setStartInput] = useState(toDatetimeLocal(now - 24 * 60 * 60 * 1_000))
  const [endInput, setEndInput] = useState(toDatetimeLocal(now))
  const [loading, setLoading] = useState(false)
  const [page, setPage] = useState(1)
  const [error, setError] = useState<string | null>(null)
  const [dashError, setDashError] = useState<string | null>(null)

  useEffect(() => {
    const controller = new AbortController()
    void getDashboard(controller.signal)
      .then((next) => {
        setDashboard(next)
        setDashError(null)
      })
      .catch((reason: unknown) => {
        if (reason instanceof DOMException && reason.name === 'AbortError') return
        setDashError(reason instanceof Error ? reason.message : String(reason))
      })
    return () => controller.abort()
  }, [])

  const accounts = dashboard?.accounts ?? []
  const query = useCallback(
    async (requestedPage = 1, signal?: AbortSignal) => {
      const startMs = fromDatetimeLocal(startInput)
      const endMs = fromDatetimeLocal(endInput)
      if (startMs == null || endMs == null) {
        setError('开始和结束时间无效')
        return
      }
      if (endMs < startMs) {
        setError('结束时间不能早于开始时间')
        return
      }
      setLoading(true)
      setError(null)
      try {
        const next = await getExecutionCost({
          startMs,
          endMs,
          windowSec,
          sourceIds: scope === 'all' ? undefined : [scope],
          strategyName: strategyName.trim() || undefined,
          page: requestedPage,
          pageSize: PAGE_SIZE,
          signal,
        })
        setSnapshot(next)
        setPage(next.report.page)
      } catch (reason: unknown) {
        if (reason instanceof DOMException && reason.name === 'AbortError') return
        setError(reason instanceof Error ? reason.message : String(reason))
      } finally {
        setLoading(false)
      }
    },
    [endInput, scope, startInput, strategyName, windowSec],
  )

  const report = snapshot?.report
  const updates = report?.updates ?? []
  const symbolRows = useMemo(() => {
    const rows: Array<{
      update: PositionUpdateExecutionCost
      sourceId: string
      shares: number
      symbol: SymbolExecutionCost
    }> = []
    for (const update of updates) {
      for (const account of update.accounts) {
        for (const symbol of account.symbols) {
          if (symbol.fill_count === 0) {
            continue
          }
          rows.push({
            update,
            sourceId: account.source_id,
            shares: account.shares,
            symbol,
          })
        }
      }
    }
    return rows
  }, [updates])

  return (
    <AppShell
      active="execution-cost"
      title="执行成本"
      subtitle="按仓位更新对比实际成交与 1 分钟 mid TWAP"
      icon={Scale}
      actions={
        <Button
          type="button"
          size="sm"
          variant="primary"
          disabled={loading}
          onClick={() => void query(1)}
        >
          {loading ? (
            <LoaderCircle size={15} className="animate-spin-slow" />
          ) : (
            <RefreshCw size={15} />
          )}
          查询生成
        </Button>
      }
    >
      <PageIntro
        eyebrow="On demand"
        title="实际价格执行 vs TWAP"
        description="仅比较有实际成交的执行窗口。实际 VWAP 与 TWAP 使用同一实际成交量计算价格滑点；正数表示成本，负数表示价格改善。手续费单独统计，不进入价格执行指标。"
      />

      {dashError && <Alert className="mb-4">账户列表失败：{dashError}</Alert>}
      {error && <Alert className="mb-4">查询失败：{error}</Alert>}

      <Card className="mb-6">
        <CardHeader>
          <CardTitle>查询条件</CardTitle>
          <CardDescription>
            到达价取更新时最近且未过期的已完成 5 秒 mid；TWAP 从该时点切连续 1 分钟 bucket（每分钟用
            5 秒 mid 平均）。成交来自 Exec RocksDB 的
            <code className="mx-1">batch_exec:&lt;strategy&gt;</code>
            归属。只统计归档时带有 published_accounts 与 shares 的仓位更新。
          </CardDescription>
        </CardHeader>
        <CardContent className="grid gap-4 md:grid-cols-2 xl:grid-cols-5">
          <Label>
            开始
            <Input
              type="datetime-local"
              value={startInput}
              onChange={(event) => setStartInput(event.target.value)}
            />
          </Label>
          <Label>
            结束
            <Input
              type="datetime-local"
              value={endInput}
              onChange={(event) => setEndInput(event.target.value)}
            />
          </Label>
          <Label>
            执行窗口
            <Select
              value={String(windowSec)}
              onChange={(event) => setWindowSec(Number(event.target.value))}
            >
              {WINDOW_OPTIONS.map((option) => (
                <option key={option.value} value={option.value}>
                  {option.label}
                </option>
              ))}
            </Select>
          </Label>
          <Label>
            账户
            <Select value={scope} onChange={(event) => setScope(event.target.value)}>
              <option value="all">全部账户</option>
              {accounts.map((account) => (
                <option key={account.source_id} value={account.source_id}>
                  {account.account}
                </option>
              ))}
            </Select>
          </Label>
          <Label>
            策略名（可空）
            <Input
              value={strategyName}
              placeholder="全部策略"
              onChange={(event) => setStrategyName(event.target.value)}
            />
          </Label>
        </CardContent>
      </Card>

      <div className="mb-6 grid gap-3 sm:grid-cols-2 xl:grid-cols-6">
        <StatTile
          label="实际价格滑点"
          value={report ? optionalBps(report.totals.actual_slippage_bps) : '--'}
          hint={report ? `${moneyU(report.totals.actual_price_slippage_usdt)} · 成交额加权` : undefined}
        />
        <StatTile
          label="TWAP 价格滑点"
          value={report ? optionalBps(report.totals.twap_slippage_bps) : '--'}
          hint={report ? `${moneyU(report.totals.twap_price_slippage_on_filled_usdt)} · 同成交量` : undefined}
        />
        <StatTile
          label="实际相对 TWAP"
          value={report ? optionalBps(report.totals.shortfall_vs_twap_bps) : '--'}
          hint={report ? `${moneyU(report.totals.shortfall_vs_twap_usdt)} · 正数较差` : undefined}
        />
        <StatTile
          label="实际手续费（独立）"
          value={report ? moneyU(report.totals.estimated_trading_fee_usdt) : '--'}
          hint="不计入任何价格滑点"
        />
        <StatTile
          label="可比成交"
          value={report ? String(report.totals.comparable_fill_count) : '--'}
          hint={report ? `${report.execution_update_count} 个执行窗口` : undefined}
        />
        <StatTile
          label="归档更新"
          value={report ? String(report.update_count) : '--'}
          hint={report ? `${snapshot?.generation_duration_ms ?? 0} ms` : undefined}
        />
      </div>

      {snapshot && report && report.points.length > 0 && (
        <Card className="mb-6">
          <CardHeader>
            <CardTitle>成交额加权价格滑点</CardTitle>
            <CardDescription>
              实际与 TWAP 使用相同的实际成交量；手续费不进入曲线。相对 TWAP 为正表示实际执行更差。
            </CardDescription>
          </CardHeader>
          <CardContent>
            <ExecutionCostChart points={report.points} />
          </CardContent>
        </Card>
      )}

      {!snapshot && !loading && (
        <Card>
          <CardContent className="py-16 text-center text-sm text-muted">
            选择时间范围后点「查询生成」。不会自动刷新。
          </CardContent>
        </Card>
      )}

      {loading && !snapshot && (
        <Card>
          <CardContent className="flex items-center justify-center gap-2 py-16 text-sm text-muted">
            <LoaderCircle size={18} className="animate-spin-slow" />
            正在从归档、TWAP 和成交生成
          </CardContent>
        </Card>
      )}

      {snapshot && (
        <Card>
          <CardHeader className="flex flex-col items-start gap-3 sm:flex-row sm:justify-between">
            <div>
              <CardTitle>每次实际执行</CardTitle>
              <CardDescription>
                窗口从该次 POST 开始，最多 {report?.window_secs} 秒，遇到同策略下一次更新提前结束。
                共 {report?.execution_update_count ?? 0} 个有成交窗口，本页{' '}
                {report?.returned_update_count ?? 0} 个。生成于 {timestampUs(snapshot.generated_at_us)}。
              </CardDescription>
            </div>
            <div className="flex shrink-0 items-center gap-2">
              <Button
                type="button"
                size="sm"
                variant="secondary"
                className="w-8 px-0"
                aria-label="上一页"
                title="上一页"
                disabled={loading || page <= 1}
                onClick={() => void query(page - 1)}
              >
                <ChevronLeft size={15} />
              </Button>
              <Badge tone="brand">
                {report?.page_count ? `${page} / ${report.page_count}` : '0 / 0'}
              </Badge>
              <Button
                type="button"
                size="sm"
                variant="secondary"
                className="w-8 px-0"
                aria-label="下一页"
                title="下一页"
                disabled={loading || !report || page >= report.page_count}
                onClick={() => void query(page + 1)}
              >
                <ChevronRight size={15} />
              </Button>
            </div>
          </CardHeader>
          <CardContent className="overflow-x-auto p-0">
            {symbolRows.length === 0 ? (
              <p className="px-5 py-10 text-center text-sm text-muted">
                这一页没有可比较的实际成交。
              </p>
            ) : (
              <table className="min-w-full text-left text-[13px]">
                <thead className="border-b border-border-soft bg-canvas/80 text-[11px] uppercase tracking-wide text-muted">
                  <tr>
                    <th className="px-4 py-2 font-medium">时间</th>
                    <th className="px-4 py-2 font-medium">策略</th>
                    <th className="px-4 py-2 font-medium">账户</th>
                    <th className="px-4 py-2 font-medium">合约</th>
                    <th className="px-4 py-2 font-medium">方向</th>
                    <th className="px-4 py-2 font-medium text-right">成交</th>
                    <th className="px-4 py-2 font-medium text-right">到达 mid</th>
                    <th className="px-4 py-2 font-medium text-right">TWAP mid</th>
                    <th className="px-4 py-2 font-medium text-right">实际 VWAP</th>
                    <th className="px-4 py-2 font-medium text-right">实际滑点</th>
                    <th className="px-4 py-2 font-medium text-right">TWAP 滑点</th>
                    <th className="px-4 py-2 font-medium text-right">相对 TWAP</th>
                    <th className="px-4 py-2 font-medium text-right">手续费 U</th>
                  </tr>
                </thead>
                <tbody>
                  {symbolRows.map((row) => {
                    const key = `${row.update.received_at_us}-${row.update.seq}-${row.sourceId}-${row.symbol.symbol}`
                    return (
                      <tr key={key} className="border-b border-border-soft last:border-0">
                        <td className="whitespace-nowrap px-4 py-2 tabular-nums text-muted">
                          {timestampUs(row.update.received_at_us)}
                          {row.update.skipped_legacy && (
                            <span className="ml-2 text-[11px] text-warning">无归档账户</span>
                          )}
                        </td>
                        <td className="px-4 py-2 font-mono text-[12px]">{row.update.strategy_name}</td>
                        <td className="px-4 py-2 font-mono text-[12px]">
                          {row.sourceId}
                          <div className="text-[11px] text-subtle">
                            ×{quantity(row.shares)} 份
                          </div>
                        </td>
                        <td className="px-4 py-2 font-medium">{row.symbol.symbol}</td>
                        <td className="px-4 py-2 font-medium">{sideLabel(row.symbol.side)}</td>
                        <td className="px-4 py-2 text-right tabular-nums">
                          {quantity(Math.abs(row.symbol.filled_qty))}
                        </td>
                        <td className="px-4 py-2 text-right tabular-nums">
                          {optionalMoney(row.symbol.arrival_mid)}
                        </td>
                        <td className="px-4 py-2 text-right tabular-nums">
                          {optionalMoney(row.symbol.twap_mid)}
                        </td>
                        <td className="px-4 py-2 text-right tabular-nums">
                          {optionalMoney(row.symbol.actual_vwap)}
                        </td>
                        <td
                          className={cn(
                            'px-4 py-2 text-right tabular-nums',
                            costClass(row.symbol.actual_slippage_bps),
                          )}
                        >
                          {optionalBps(row.symbol.actual_slippage_bps)}
                        </td>
                        <td
                          className={cn(
                            'px-4 py-2 text-right tabular-nums',
                            costClass(row.symbol.twap_slippage_bps),
                          )}
                        >
                          {optionalBps(row.symbol.twap_slippage_bps)}
                        </td>
                        <td
                          className={cn(
                            'px-4 py-2 text-right tabular-nums',
                            costClass(row.symbol.shortfall_vs_twap_bps),
                          )}
                        >
                          {optionalBps(row.symbol.shortfall_vs_twap_bps)}
                        </td>
                        <td
                          className={cn(
                            'px-4 py-2 text-right tabular-nums',
                            costClass(row.symbol.estimated_trading_fee_usdt),
                          )}
                        >
                          {money(row.symbol.estimated_trading_fee_usdt)}
                        </td>
                      </tr>
                    )
                  })}
                </tbody>
              </table>
            )}
          </CardContent>
        </Card>
      )}
    </AppShell>
  )
}
