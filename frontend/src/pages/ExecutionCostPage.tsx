import { LoaderCircle, RefreshCw, Scale } from 'lucide-react'
import { useCallback, useEffect, useMemo, useState } from 'react'
import { getDashboard, getExecutionCost } from '../api'
import { AppShell, PageIntro, StatTile } from '../components/AppShell'
import { Alert, Badge } from '../components/ui/Badge'
import { Button } from '../components/ui/Button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '../components/ui/Card'
import { Input, Label, Select } from '../components/ui/Field'
import { money, quantity, signedClass, timestampUs } from '../format'
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

function costClass(value: number | null | undefined) {
  if (value == null) return ''
  return signedClass(value)
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
    async (signal?: AbortSignal) => {
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
          signal,
        })
        setSnapshot(next)
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
          if (Math.abs(symbol.intended_qty) < 1e-12 && Math.abs(symbol.filled_qty) < 1e-12) {
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
          onClick={() => void query()}
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
        title="实际成交 vs TWAP 预估"
        description="每次仓位 POST 的目标仓位按当时归档的份数减去快照仓位，得到要执行的数量。默认假设 5 分钟均匀执行：从这次更新起切 5 个 1 分钟，每分钟对其中 5 秒 mid 等权平均，再对这 5 个 1 分钟 mid 平均，得到 TWAP 预估费前，并和同一窗口实际成交 VWAP 的费前成本对比。不是实时任务，点查询才生成。"
      />

      {dashError && <Alert className="mb-4">账户列表失败：{dashError}</Alert>}
      {error && <Alert className="mb-4">查询失败：{error}</Alert>}

      <Card className="mb-6">
        <CardHeader>
          <CardTitle>查询条件</CardTitle>
          <CardDescription>
            价格基准是从这次更新起的连续 1 分钟 mid（每分钟用 5 秒 mid 平均）；成交来自 Exec RocksDB 的
            <code className="mx-1">batch_exec:&lt;strategy&gt;</code>
            归属。归档里没有账户份数的旧消息会跳过。
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

      <div className="mb-6 grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
        <StatTile
          label="实际费前成本"
          value={report ? money(report.totals.actual_cost_before_fee_usdt) : '--'}
          hint="filled × (VWAP − arrival mid)"
        />
        <StatTile
          label="TWAP 预估费前"
          value={report ? money(report.totals.twap_cost_before_fee_usdt) : '--'}
          hint="intended × (5×1m mid 平均 − arrival mid)"
        />
        <StatTile
          label="仓位更新"
          value={report ? String(report.update_count) : '--'}
          hint={
            report
              ? `跳过旧消息 ${report.skipped_legacy_update_count} · ${snapshot?.generation_duration_ms ?? 0} ms`
              : undefined
          }
        />
        <StatTile
          label="成交 / 意向"
          value={
            report
              ? `${quantity(report.totals.filled_qty)} / ${quantity(report.totals.intended_qty)}`
              : '--'
          }
        />
      </div>

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
          <CardHeader className="flex flex-row items-start justify-between gap-3">
            <div>
              <CardTitle>每次仓位更新</CardTitle>
              <CardDescription>
                窗口从该次 POST 开始，最多 {report?.window_secs} 秒，遇到同策略下一次更新提前结束。
                生成于 {timestampUs(snapshot.generated_at_us)}。
              </CardDescription>
            </div>
            <Badge tone="brand">1m mid TWAP · 费前</Badge>
          </CardHeader>
          <CardContent className="overflow-x-auto p-0">
            {symbolRows.length === 0 ? (
              <p className="px-5 py-10 text-center text-sm text-muted">
                这段时间没有可估算的仓位更新。旧消息缺少账户份数会被跳过。
              </p>
            ) : (
              <table className="min-w-full text-left text-[13px]">
                <thead className="border-b border-border-soft bg-canvas/80 text-[11px] uppercase tracking-wide text-muted">
                  <tr>
                    <th className="px-4 py-2 font-medium">时间</th>
                    <th className="px-4 py-2 font-medium">策略</th>
                    <th className="px-4 py-2 font-medium">账户</th>
                    <th className="px-4 py-2 font-medium">合约</th>
                    <th className="px-4 py-2 font-medium text-right">意向</th>
                    <th className="px-4 py-2 font-medium text-right">成交</th>
                    <th className="px-4 py-2 font-medium text-right">到达 mid</th>
                    <th className="px-4 py-2 font-medium text-right">TWAP mid</th>
                    <th className="px-4 py-2 font-medium text-right">实际 VWAP</th>
                    <th className="px-4 py-2 font-medium text-right">TWAP 费前</th>
                    <th className="px-4 py-2 font-medium text-right">实际费前</th>
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
                            <span className="ml-2 text-[11px] text-warning">旧消息</span>
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
                        <td className="px-4 py-2 text-right tabular-nums">
                          {quantity(row.symbol.intended_qty)}
                        </td>
                        <td className="px-4 py-2 text-right tabular-nums">
                          {quantity(row.symbol.filled_qty)}
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
                            costClass(row.symbol.twap_cost_before_fee_usdt),
                          )}
                        >
                          {optionalMoney(row.symbol.twap_cost_before_fee_usdt)}
                        </td>
                        <td
                          className={cn(
                            'px-4 py-2 text-right tabular-nums',
                            costClass(row.symbol.actual_cost_before_fee_usdt),
                          )}
                        >
                          {optionalMoney(row.symbol.actual_cost_before_fee_usdt)}
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
