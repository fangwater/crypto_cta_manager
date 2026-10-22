import { theoreticalExecutionDescription, theoreticalExecutionLabel } from '../lib/theoreticalExecution'
import { ChevronLeft, ChevronRight, LoaderCircle, RefreshCw, Scale } from 'lucide-react'
import { useCallback, useEffect, useRef, useState } from 'react'
import { getAcquisitionCost, getDashboard, getKlineCacheStatus } from '../api'
import type { KlineCacheStatus } from '../api'
import { AppShell, PageIntro, StatTile } from '../components/AppShell'
import { AcquisitionDeltaAnalysis, AcquisitionOverview } from '../components/AcquisitionCostCharts'
import { Alert, Badge } from '../components/ui/Badge'
import { Button } from '../components/ui/Button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '../components/ui/Card'
import { Input, Label, Select } from '../components/ui/Field'
import { money, quantity, timestampUs } from '../format'
import { cn } from '../lib/cn'
import { readSourceId } from '../lib/routes'
import { deltaKey, priceFormatter, rowPriceFormatter } from '../lib/acquisitionAnalysis'
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
  const formatPrice = rowPriceFormatter(row)
  return row.sample_prices.map((value) => value == null ? '无成交' : formatPrice(value)).join(' / ')
}

export function AcquisitionCostPage() {
  const initialSource = readSourceId()
  const now = Date.now()
  const [dashboard, setDashboard] = useState<DashboardSnapshot | null>(null)
  const [snapshot, setSnapshot] = useState<AcquisitionCostSnapshot | null>(null)
  const [scope, setScope] = useState(initialSource)
  const [strategyName, setStrategyName] = useState('')
  const [startInput, setStartInput] = useState(toDatetimeLocal(now - 24 * 60 * 60 * 1_000))
  const [endInput, setEndInput] = useState(toDatetimeLocal(now))
  const [page, setPage] = useState(1)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [cacheStatus, setCacheStatus] = useState<KlineCacheStatus | null>(null)
  const [referenceIndex, setReferenceIndex] = useState(0)
  const [selectedKey, setSelectedKey] = useState('')
  const [rowSymbol, setRowSymbol] = useState('')
  const [rowStrategy, setRowStrategy] = useState('')
  const [rowSort, setRowSort] = useState<'latest' | 'price_shortfall'>('latest')
  const queryController = useRef<AbortController | null>(null)
  const scopeReady = Boolean(dashboard?.accounts?.some((account) => account.enabled && account.source_id === scope))

  useEffect(() => {
    queryController.current?.abort()
    setLoading(false)
    setSnapshot(null)
    setSelectedKey('')
    setPage(1)
    return () => queryController.current?.abort()
  }, [scope, startInput, endInput, strategyName])

  useEffect(() => {
    const controller = new AbortController()
    const refresh = () => { void getKlineCacheStatus(controller.signal).then(setCacheStatus).catch(() => {}) }
    refresh()
    const interval = window.setInterval(refresh, 10_000)
    return () => { controller.abort(); window.clearInterval(interval) }
  }, [])

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

  useEffect(() => {
    if (!dashboard) return
    const accounts = (dashboard.accounts ?? []).filter((account) => account.enabled)
    const nextScope = accounts.some((account) => account.source_id === scope)
      ? scope
      : accounts[0]?.source_id ?? ''
    if (scope !== nextScope) {
      setScope(nextScope)
      setSnapshot(null)
      setPage(1)
    }
    const url = new URL(window.location.href)
    if (nextScope) url.searchParams.set('source', nextScope)
    else url.searchParams.delete('source')
    window.history.replaceState(null, '', `${url.pathname}${url.search}${url.hash}`)
  }, [dashboard, scope])

  function selectScope(nextScope: string) {
    setScope(nextScope)
    setSnapshot(null)
    setPage(1)
    const url = new URL(window.location.href)
    url.searchParams.set('source', nextScope)
    window.history.replaceState(null, '', `${url.pathname}${url.search}${url.hash}`)
  }

  const query = useCallback(
    async (requestedPage = 1, nextSymbol = rowSymbol, nextSort = rowSort, nextStrategy = rowStrategy) => {
      if (!scopeReady) return
      const startMs = fromDatetimeLocal(startInput)
      const endMs = fromDatetimeLocal(endInput)
      if (startMs == null || endMs == null || endMs < startMs) {
        setError('时间范围无效')
        return
      }
      setLoading(true)
      setError(null)
      queryController.current?.abort()
      const controller = new AbortController()
      queryController.current = controller
      try {
        const next = await getAcquisitionCost({
          startMs,
          endMs,
          sourceIds: [scope],
          strategyName: strategyName.trim() || undefined,
          rowSymbol: nextSymbol || undefined,
          rowStrategy: nextStrategy || undefined,
          rowSort: nextSort,
          page: requestedPage,
          pageSize: PAGE_SIZE,
          signal: controller.signal,
        })
        if (controller.signal.aborted) return
        setSnapshot(next)
        const lastRow = next.reports[referenceIndex]?.rows.at(-1)
        setSelectedKey(lastRow ? deltaKey(lastRow) : '')
        setPage(next.reports[referenceIndex]?.page ?? 1)
      } catch (reason: unknown) {
        if (!controller.signal.aborted) setError(reason instanceof Error ? reason.message : String(reason))
      } finally {
        if (queryController.current === controller) setLoading(false)
      }
    },
    [referenceIndex, endInput, rowSort, rowStrategy, rowSymbol, scope, scopeReady, startInput, strategyName],
  )

  function selectDetails(symbol: string, sort: 'latest' | 'price_shortfall', strategy = rowStrategy) {
    setRowSymbol(symbol)
    setRowSort(sort)
    setRowStrategy(strategy)
    void query(1, symbol, sort, strategy)
    document.getElementById('delta-details')?.scrollIntoView({ behavior: 'smooth', block: 'start' })
  }

  const report = snapshot?.reports[referenceIndex] ?? snapshot?.reports[0]
  const totals = report?.totals
  return (
    <AppShell
      active="acquisition-cost"
      title="持仓成本"
      subtitle={`实际成交与${theoreticalExecutionLabel(report?.execution_model)}`}
      icon={Scale}
      actions={
        <Button type="button" size="sm" variant="primary" disabled={loading || !scopeReady} onClick={() => void query(1)}>
          {loading ? <LoaderCircle size={15} className="animate-spin-slow" /> : <RefreshCw size={15} />}
          查询生成
        </Button>
      }
    >
      <PageIntro
        eyebrow="Delta cost"
        title="实际成本 vs 虚拟成本"
        description={theoreticalExecutionDescription(report?.execution_model)}
      />

      {error && <Alert className="mb-4">{error}</Alert>}
      {cacheStatus?.enabled && <div className="mb-4 text-sm text-muted" role="status">
        行情缓存：已补拉 {cacheStatus.fetched_candles.toLocaleString()} 根分钟 K 线 · {cacheStatus.active_backfills ? `${cacheStatus.active_backfills} 个币对正在补齐，可稍后重新查询` : '当前没有补拉任务'}
      </div>}
      {report?.warnings.map((warning) => <Alert key={warning} className="mb-4">{warning}</Alert>)}
      {Boolean(totals?.pending_virtual_delta_count) && <Alert className="mb-4">{totals?.pending_virtual_delta_count} 个 delta 的理论执行窗口尚未结束。</Alert>}
      {Boolean(totals?.zero_volume_skipped_sample_count) && <div className="mb-4 text-sm text-muted" role="status">跳过 {totals?.zero_volume_skipped_sample_count} 个无成交分钟样本，{report?.execution_model === 'vwap3m' ? '其余分钟按市场成交量占比分配理论数量。' : '其余分钟按账户理论执行规则分配数量。'}</div>}
      {Boolean(totals?.zero_volume_fallback_sample_count) && <Alert className="mb-4">{totals?.zero_volume_fallback_delta_count} 个目标的五分钟全部无成交，使用收盘价定价。</Alert>}
      {totals && totals.missing_virtual_delta_count > 0 && (
        <Alert className="mb-4">
          {totals.missing_virtual_delta_count} 个 delta 缺少可定价的完整窗口行情，未进入可比成本。
        </Alert>
      )}
      {totals && totals.stale_reference_fill_count > 0 && (
        <Alert className="mb-4">
          {totals.stale_reference_fill_count} 笔事实 fill 的最近虚拟参考超过 300 秒，成交额{' '}
          {moneyU(totals.stale_reference_fill_notional_usdt)}，未进入价格差。
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
            <Select value={scope} disabled={loading || !scopeReady} onChange={(event) => selectScope(event.target.value)}>
              {!scopeReady && <option value="">暂无可查看账户</option>}
              {(dashboard?.accounts ?? []).filter((account) => account.enabled).map((account) => (
                <option key={account.source_id} value={account.source_id}>{account.account}</option>
              ))}
            </Select>
          </Label>
          <Label>
            策略（查询范围）
            <Input placeholder="留空查询全部策略" value={strategyName} onChange={(event) => setStrategyName(event.target.value)} />
          </Label>
        </CardContent>
      </Card>

      {(snapshot?.reports.length ?? 0) > 1 && <Card className="mb-6">
        <CardHeader><CardTitle>两个三分钟参考</CardTitle><CardDescription>同一查询范围、实际成交和费用，分别计算参考价格与滑点。选择参考可查看对应的图表和逐 delta 明细。</CardDescription></CardHeader>
        <CardContent className="grid gap-4 sm:grid-cols-2">{snapshot?.reports.map((reference, index) => <div key={reference.execution_model} className="rounded-lg border border-border-soft p-4">
          <Button variant={referenceIndex === index ? 'primary' : 'secondary'} onClick={() => setReferenceIndex(index)}>{theoreticalExecutionLabel(reference.execution_model)}</Button>
          <div className="mt-3 text-sm">费前滑点 {reference.totals.matched_fill_count ? bps(reference.totals.price_shortfall_bps) : '--'} · 费后滑点 {reference.totals.matched_fill_count ? bps(reference.totals.after_fee_shortfall_bps) : '--'}</div>
          <div className="mt-2 text-xs text-muted">可比成交 {reference.totals.matched_fill_count} 笔 · 缺价 {reference.totals.missing_virtual_delta_count} 个 delta</div>
        </div>)}</CardContent>
      </Card>}
      <div className="mb-6 grid gap-3 sm:grid-cols-2 xl:grid-cols-6">
        <StatTile label="目标虚拟成交额" value={totals ? moneyU(totals.virtual_turnover_usdt) : '--'} hint={totals ? `${totals.virtual_delta_count} 个 delta · 费 ${moneyU(totals.virtual_fee_usdt)}` : undefined} />
        <StatTile label="事实成交额" value={totals ? moneyU(totals.actual_turnover_usdt) : '--'} hint={totals ? `${totals.actual_fill_count} 笔 · 费 ${moneyU(totals.actual_fee_usdt)}` : undefined} />
        <StatTile label="同量虚拟成交额" value={totals ? moneyU(totals.matched_virtual_turnover_usdt) : '--'} hint="完全使用事实 fill 数量" />
        <StatTile label="事实成交覆盖" value={totals ? `${(totals.actual_fill_reference_coverage * 100).toFixed(1)}%` : '--'} />
        <StatTile label="费前执行滑点" value={totals?.matched_fill_count ? bps(totals.price_shortfall_bps) : '--'} hint={totals ? `价格差 ${moneyU(totals.price_shortfall_usdt)} · 按可比理论成交额归一化` : undefined} />
        <StatTile label="费后执行滑点" value={totals?.matched_fill_count ? bps(totals.after_fee_shortfall_bps) : '--'} hint={totals ? `费后差 ${moneyU(totals.after_fee_shortfall_usdt)} · 手续费差 ${moneyU(totals.fee_shortfall_usdt)}` : undefined} />
      </div>

      {!snapshot && !loading && <Card><CardContent className="py-16 text-center text-sm text-muted">选择范围后查询。</CardContent></Card>}
      {loading && !snapshot && <Card><CardContent className="flex items-center justify-center gap-2 py-16 text-sm text-muted"><LoaderCircle size={18} className="animate-spin-slow" />正在生成成本账本</CardContent></Card>}

      {report && <>
        <AcquisitionOverview report={report} onSelectSymbol={(symbol) => selectDetails(symbol, 'price_shortfall')} />
        <Card className="mb-4 scroll-mt-4" id="delta-details">
          <CardHeader>
            <CardTitle>定位 delta</CardTitle>
            <CardDescription>按币种和策略在整个查询区间内先筛选、排序，再分页；上方汇总保持全区间口径。单个目标复盘仅列出当前页目标。</CardDescription>
          </CardHeader>
          <CardContent className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
            <Label>复盘币种<Select value={rowSymbol} disabled={loading} onChange={(event) => selectDetails(event.target.value, rowSort)}>
              <option value="">全部币种</option>
              {rowSymbol && !report.by_symbol.some((row) => row.bucket === rowSymbol) && <option value={rowSymbol}>{rowSymbol}</option>}
              {[...report.by_symbol].sort((a, b) => a.bucket.localeCompare(b.bucket)).map((row) => <option key={row.bucket} value={row.bucket}>{row.bucket}</option>)}
            </Select></Label>
            <Label>复盘策略<Select value={rowStrategy} disabled={loading} onChange={(event) => selectDetails(rowSymbol, rowSort, event.target.value)}>
              <option value="">全部策略</option>
              {rowStrategy && !report.by_strategy.some((row) => row.bucket === rowStrategy) && <option value={rowStrategy}>{rowStrategy}</option>}
              {[...report.by_strategy].sort((a, b) => a.bucket.localeCompare(b.bucket)).map((row) => <option key={row.bucket} value={row.bucket}>{row.bucket}</option>)}
            </Select></Label>
            <Label>目标排序<Select value={rowSort} disabled={loading} onChange={(event) => selectDetails(rowSymbol, event.target.value as 'latest' | 'price_shortfall')}>
              <option value="latest">发布时间最新优先</option>
              <option value="price_shortfall">价格差金额最大优先（正值更差）</option>
            </Select></Label>
            <div className="flex flex-wrap items-center justify-between gap-3 sm:col-span-2 lg:col-span-3">
              <span className="text-sm text-muted" role="status">{loading ? '正在载入目标明细…' : `${report.row_symbol ?? '全部币种'} · ${report.row_strategy ?? '全部策略'} · ${report.filtered_row_count} 个 delta · 当前页 ${report.returned_row_count} 个`}</span>
              <div className="flex items-center gap-2">
                <Button type="button" size="sm" variant="secondary" disabled={loading || page <= 1} onClick={() => void query(page - 1)}>上一页</Button>
                <Badge tone="brand">{report.page_count ? `${page} / ${report.page_count}` : '0 / 0'}</Badge>
                <Button type="button" size="sm" variant="secondary" disabled={loading || page >= report.page_count} onClick={() => void query(page + 1)}>下一页</Button>
              </div>
            </div>
          </CardContent>
        </Card>
        {!loading && (report.rows.length
          ? <AcquisitionDeltaAnalysis report={report} selectedKey={selectedKey} onSelect={setSelectedKey} />
          : <Alert className="mb-6">当前币种、策略和范围没有可展示的 delta，可切换复盘条件或扩大上方查询范围。</Alert>)}
      </>}

      {report && (
        <Card className="mb-6">
          <CardHeader><CardTitle>策略对比</CardTitle><CardDescription>正值表示实际成交更贵；先看覆盖率，再比较价格差和费用差。</CardDescription></CardHeader>
          <CardContent className="overflow-x-auto">
            <table className="min-w-full text-left text-[13px]">
              <thead><tr><th>策略</th><th className="text-right">可比成交数</th><th className="text-right">可比成交额 U</th><th className="text-right">价格差 bps</th><th className="text-right">费后差 U</th></tr></thead>
              <tbody>{report.by_strategy.map((row) => <tr key={row.bucket} className="border-b border-border-soft">
                <td className="py-2">{row.bucket}</td><td className="text-right tabular-nums">{row.fill_count}</td>
                <td className="text-right tabular-nums">{money(row.reference_turnover_usdt)}</td>
                <td className={cn('text-right tabular-nums', costClass(row.price_shortfall_bps))}>{bps(row.price_shortfall_bps)}</td>
                <td className={cn('text-right tabular-nums', costClass(row.after_fee_shortfall_usdt))}>{moneyU(row.after_fee_shortfall_usdt)}</td>
              </tr>)}</tbody>
            </table>
          </CardContent>
        </Card>
      )}
      {report && (
        <Card>
          <CardHeader className="flex flex-col items-start gap-3 sm:flex-row sm:justify-between">
            <div>
              <CardTitle>逐 delta 成本</CardTitle>
              <CardDescription>点击“复盘”查看价格路径和成交点。价格按数值及价差自适应精度；价格差 bps 已按买卖方向调整，正值更差。完成率按实际配对数量除以 delta 数量。</CardDescription>
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
                  <th className="px-4 py-2 font-medium">信号 / 策略</th><th className="px-4 py-2 font-medium">合约</th><th className="px-4 py-2 font-medium">方向</th><th className="px-4 py-2 text-right font-medium">Delta</th><th className="px-4 py-2 text-right font-medium">分钟价格</th><th className="px-4 py-2 text-right font-medium">计价分钟</th><th className="px-4 py-2 text-right font-medium">{(snapshot?.reports.length ?? 0) > 1 ? "三分钟参考价" : theoreticalExecutionLabel(report?.execution_model)}</th><th className="px-4 py-2 text-right font-medium">实际 VWAP</th><th className="px-4 py-2 text-right font-medium">实际 − 理论</th><th className="px-4 py-2 text-right font-medium">完成率</th><th className="px-4 py-2 text-right font-medium">价格差</th><th className="px-4 py-2 text-right font-medium">费后差 U</th>
                </tr>
              </thead>
              <tbody>
              {[...report.rows].reverse().map((row) => {
                  const formatPrice = rowPriceFormatter(row)
                  const difference = row.actual_vwap == null ? null : row.actual_vwap - row.virtual_vwap
                  return <tr key={deltaKey(row)} className={cn('border-b border-border-soft last:border-0', selectedKey === deltaKey(row) && 'bg-canvas')}>
                    <td className="whitespace-nowrap px-4 py-2 tabular-nums text-muted">{timestampUs(row.received_at_us)}<div className="text-xs">{row.strategy_name} <button type="button" className="ml-2 text-brand underline" onClick={() => { setSelectedKey(deltaKey(row)); document.getElementById('delta-analysis')?.scrollIntoView({ behavior: 'smooth', block: 'start' }) }}>复盘</button></div></td>
                    <td className="px-4 py-2 font-medium">{row.symbol}</td>
                    <td className="px-4 py-2">{side(row.delta_qty)}</td>
                    <td className="px-4 py-2 text-right tabular-nums">{quantity(Math.abs(row.delta_qty))}</td>
                    <td className="whitespace-nowrap px-4 py-2 text-right text-[11px] tabular-nums text-muted">{sampleText(row)}</td>
                    <td className="px-4 py-2 text-right tabular-nums">{row.zero_volume_fallback_sample_count ? "收盘价" : (row.sample_prices.length - row.zero_volume_skipped_sample_count) + " / " + row.sample_prices.length}</td>
                    <td className="whitespace-nowrap px-4 py-2 text-right tabular-nums">{row.reference_prices.map((reference) => <div key={reference.execution_model}>{row.reference_prices.length > 1 && <span className="mr-2 text-xs text-muted">{theoreticalExecutionLabel(reference.execution_model)}</span>}{formatPrice(reference.price)}</div>)}</td>
                    <td className="whitespace-nowrap px-4 py-2 text-right tabular-nums">{formatPrice(row.actual_vwap)}</td>
                    <td className="whitespace-nowrap px-4 py-2 text-right tabular-nums">{difference != null && difference > 0 ? '+' : ''}{priceFormatter([difference])(difference)}</td>
                    <td className="px-4 py-2 text-right tabular-nums">{(row.fill_ratio * 100).toFixed(1)}%</td>
                    <td className={cn('px-4 py-2 text-right tabular-nums', costClass(row.price_shortfall_bps))}>{bps(row.price_shortfall_bps)}</td>
                    <td className={cn('px-4 py-2 text-right tabular-nums', costClass(row.after_fee_shortfall_usdt))}>{moneyU(row.after_fee_shortfall_usdt)}</td>
                  </tr>
                })}
              </tbody>
            </table>
          </CardContent>
        </Card>
      )}
    </AppShell>
  )
}
