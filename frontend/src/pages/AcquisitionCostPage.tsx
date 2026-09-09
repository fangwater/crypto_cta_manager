import { ChevronLeft, ChevronRight, LoaderCircle, RefreshCw, Scale } from 'lucide-react'
import { useCallback, useEffect, useState } from 'react'
import { getAcquisitionCost, getDashboard } from '../api'
import { AppShell, PageIntro, StatTile } from '../components/AppShell'
import { Alert, Badge } from '../components/ui/Badge'
import { Button } from '../components/ui/Button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '../components/ui/Card'
import { Input, Label, Select } from '../components/ui/Field'
import { money, quantity, timestampUs } from '../format'
import { cn } from '../lib/cn'
import { readSourceId } from '../lib/routes'
import type { AcquisitionCostRow, AcquisitionCostSnapshot, DashboardSnapshot } from '../types'

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

function moneyU(value: number | null | undefined) {
  return value == null ? '--' : `${money(value)} U`
}

function bps(value: number | null | undefined) {
  return value == null ? '--' : `${value.toFixed(2)} bps`
}

function costClass(value: number | null | undefined) {
  if (value == null || value === 0) return ''
  return value > 0 ? 'number-negative' : 'number-positive'
}

function side(delta: number) {
  return delta > 0 ? '买' : '卖'
}

function sampleText(row: AcquisitionCostRow) {
  return row.sample_mids.map((value) => money(value)).join(' / ')
}

export function AcquisitionCostPage() {
  const initialSource = readSourceId()
  const now = Date.now()
  const [dashboard, setDashboard] = useState<DashboardSnapshot | null>(null)
  const [snapshot, setSnapshot] = useState<AcquisitionCostSnapshot | null>(null)
  const [scope, setScope] = useState(initialSource || 'all')
  const [strategyName, setStrategyName] = useState('rbf_small')
  const [startInput, setStartInput] = useState(toDatetimeLocal(now - 24 * 60 * 60 * 1_000))
  const [endInput, setEndInput] = useState(toDatetimeLocal(now))
  const [page, setPage] = useState(1)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    const controller = new AbortController()
    void getDashboard(controller.signal)
      .then(setDashboard)
      .catch((reason: unknown) => {
        if (!(reason instanceof DOMException && reason.name === 'AbortError')) {
          setError(reason instanceof Error ? reason.message : String(reason))
        }
      })
    return () => controller.abort()
  }, [])

  const query = useCallback(
    async (requestedPage = 1) => {
      const startMs = fromDatetimeLocal(startInput)
      const endMs = fromDatetimeLocal(endInput)
      if (startMs == null || endMs == null || endMs < startMs) {
        setError('时间范围无效')
        return
      }
      setLoading(true)
      setError(null)
      try {
        const next = await getAcquisitionCost({
          startMs,
          endMs,
          sourceIds: scope === 'all' ? undefined : [scope],
          strategyName: strategyName.trim() || undefined,
          page: requestedPage,
          pageSize: PAGE_SIZE,
        })
        setSnapshot(next)
        setPage(next.report.page)
      } catch (reason: unknown) {
        setError(reason instanceof Error ? reason.message : String(reason))
      } finally {
        setLoading(false)
      }
    },
    [endInput, scope, startInput, strategyName],
  )

  const report = snapshot?.report
  const totals = report?.totals
  return (
    <AppShell
      active="execution-cost"
      title="持仓成本"
      subtitle="实际成交与五切片虚拟成交"
      icon={Scale}
      actions={
        <Button type="button" size="sm" variant="primary" disabled={loading} onClick={() => void query(1)}>
          {loading ? <LoaderCircle size={15} className="animate-spin-slow" /> : <RefreshCw size={15} />}
          查询生成
        </Button>
      }
    >
      <PageIntro
        eyebrow="Delta cost"
        title="实际成本 vs 虚拟成本"
        description="每次目标变化冻结 delta，按五个相隔 60 秒的 5 秒 mid 等量模拟成交。实际成交按同方向和 delta 数量上限配对。"
      />

      {error && <Alert className="mb-4">{error}</Alert>}
      {totals && totals.missing_virtual_delta_count > 0 && (
        <Alert className="mb-4">
          {totals.missing_virtual_delta_count} 个 delta 缺少完整五点行情，未进入可比成本。
        </Alert>
      )}

      <Card className="mb-6">
        <CardHeader>
          <CardTitle>查询条件</CardTitle>
          <CardDescription>成本查询不使用 mark price。正的 shortfall 表示实际成交更差。</CardDescription>
        </CardHeader>
        <CardContent className="grid gap-4 sm:grid-cols-2 xl:grid-cols-4">
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
            <Select value={scope} onChange={(event) => setScope(event.target.value)}>
              <option value="all">全部账户</option>
              {(dashboard?.accounts ?? []).map((account) => (
                <option key={account.source_id} value={account.source_id}>{account.account}</option>
              ))}
            </Select>
          </Label>
          <Label>
            策略
            <Input value={strategyName} onChange={(event) => setStrategyName(event.target.value)} />
          </Label>
        </CardContent>
      </Card>

      <div className="mb-6 grid gap-3 sm:grid-cols-2 xl:grid-cols-6">
        <StatTile label="目标虚拟成交额" value={totals ? moneyU(totals.virtual_turnover_usdt) : '--'} hint={totals ? `${totals.virtual_delta_count} 个 delta · 费 ${moneyU(totals.virtual_fee_usdt)}` : undefined} />
        <StatTile label="事实成交额" value={totals ? moneyU(totals.actual_turnover_usdt) : '--'} hint={totals ? `${totals.actual_fill_count} 笔 · 费 ${moneyU(totals.actual_fee_usdt)}` : undefined} />
        <StatTile label="同量虚拟成交额" value={totals ? moneyU(totals.matched_virtual_turnover_usdt) : '--'} hint="完全使用事实 fill 数量" />
        <StatTile label="事实成交覆盖" value={totals ? `${(totals.actual_fill_reference_coverage * 100).toFixed(1)}%` : '--'} />
        <StatTile label="价格差" value={totals ? bps(totals.price_shortfall_bps) : '--'} hint={totals ? moneyU(totals.price_shortfall_usdt) : undefined} />
        <StatTile label="费后差" value={totals ? moneyU(totals.after_fee_shortfall_usdt) : '--'} hint={totals ? `手续费差 ${moneyU(totals.fee_shortfall_usdt)}` : undefined} />
      </div>

      {!snapshot && !loading && <Card><CardContent className="py-16 text-center text-sm text-muted">选择范围后查询。</CardContent></Card>}
      {loading && !snapshot && <Card><CardContent className="flex items-center justify-center gap-2 py-16 text-sm text-muted"><LoaderCircle size={18} className="animate-spin-slow" />正在生成成本账本</CardContent></Card>}

      {report && (
        <Card>
          <CardHeader className="flex flex-col items-start gap-3 sm:flex-row sm:justify-between">
            <div>
              <CardTitle>逐 delta 成本</CardTitle>
              <CardDescription>虚拟价格保存全部五个 mid；完成率按实际配对数量除以 delta 数量。</CardDescription>
            </div>
            <div className="flex items-center gap-2">
              <Button type="button" size="sm" variant="secondary" className="w-8 px-0" title="上一页" aria-label="上一页" disabled={loading || page <= 1} onClick={() => void query(page - 1)}><ChevronLeft size={15} /></Button>
              <Badge tone="brand">{report.page_count ? `${page} / ${report.page_count}` : '0 / 0'}</Badge>
              <Button type="button" size="sm" variant="secondary" className="w-8 px-0" title="下一页" aria-label="下一页" disabled={loading || page >= report.page_count} onClick={() => void query(page + 1)}><ChevronRight size={15} /></Button>
            </div>
          </CardHeader>
          <CardContent className="overflow-x-auto p-0">
            <table className="min-w-full text-left text-[13px]">
              <thead className="border-b border-border-soft bg-canvas/80 text-[11px] uppercase tracking-wide text-muted">
                <tr>
                  <th className="px-4 py-2 font-medium">信号</th><th className="px-4 py-2 font-medium">合约</th><th className="px-4 py-2 font-medium">方向</th><th className="px-4 py-2 text-right font-medium">Delta</th><th className="px-4 py-2 text-right font-medium">五个 mid</th><th className="px-4 py-2 text-right font-medium">虚拟均价</th><th className="px-4 py-2 text-right font-medium">实际 VWAP</th><th className="px-4 py-2 text-right font-medium">完成率</th><th className="px-4 py-2 text-right font-medium">价格差</th><th className="px-4 py-2 text-right font-medium">费后差 U</th>
                </tr>
              </thead>
              <tbody>
                {report.rows.map((row) => (
                  <tr key={`${row.source_id}-${row.binding_name}-${row.received_at_us}-${row.symbol}`} className="border-b border-border-soft last:border-0">
                    <td className="whitespace-nowrap px-4 py-2 tabular-nums text-muted">{timestampUs(row.received_at_us)}</td>
                    <td className="px-4 py-2 font-medium">{row.symbol}</td>
                    <td className="px-4 py-2">{side(row.delta_qty)}</td>
                    <td className="px-4 py-2 text-right tabular-nums">{quantity(Math.abs(row.delta_qty))}</td>
                    <td className="whitespace-nowrap px-4 py-2 text-right text-[11px] tabular-nums text-muted">{sampleText(row)}</td>
                    <td className="px-4 py-2 text-right tabular-nums">{money(row.virtual_vwap)}</td>
                    <td className="px-4 py-2 text-right tabular-nums">{row.actual_vwap == null ? '--' : money(row.actual_vwap)}</td>
                    <td className="px-4 py-2 text-right tabular-nums">{(row.fill_ratio * 100).toFixed(1)}%</td>
                    <td className={cn('px-4 py-2 text-right tabular-nums', costClass(row.price_shortfall_bps))}>{bps(row.price_shortfall_bps)}</td>
                    <td className={cn('px-4 py-2 text-right tabular-nums', costClass(row.after_fee_shortfall_usdt))}>{moneyU(row.after_fee_shortfall_usdt)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </CardContent>
        </Card>
      )}
    </AppShell>
  )
}
