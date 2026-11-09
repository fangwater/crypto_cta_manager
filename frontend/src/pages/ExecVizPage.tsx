import { LoaderCircle, Monitor, RefreshCw } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
import { ApiError, getDashboard, getTimeline } from '../api'
import { AppShell, PageIntro, StatTile } from '../components/AppShell'
import { Alert, Badge } from '../components/ui/Badge'
import { Button } from '../components/ui/Button'
import { UnifiedExecutionMonitor } from '../components/UnifiedExecutionMonitor'
import { Select } from '../components/ui/Field'
import { cn } from '../lib/cn'
import { executionGroups, groupAccount, requireGroupSources } from '../lib/executionGroups'
import { routes } from '../lib/routes'
import { fundingMoney, money, signedClass, strategyLabel, timestampUs } from '../format'
import type { DashboardSnapshot, TimelineSnapshot } from '../types'

export function ExecVizPage() {
  const requestedGroup = new URLSearchParams(location.search).get('group') ?? ''
  const [dashboard, setDashboard] = useState<DashboardSnapshot | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [showPnl, setShowPnl] = useState(false)
  const [range, setRange] = useState('1')
  const [revision, setRevision] = useState(0)
  const [pnl, setPnl] = useState<TimelineSnapshot | null>(null)
  const [pnlLoading, setPnlLoading] = useState(false)
  const [pnlError, setPnlError] = useState<string | null>(null)
  useEffect(() => {
    const controller = new AbortController()
    const load = () => { void getDashboard(controller.signal).then(value => { setDashboard(value); setError(null) }).catch(reason => { if (!controller.signal.aborted) { setError(String(reason)); if (reason instanceof ApiError && [401, 403].includes(reason.status)) setDashboard(null) } }) }
    load()
    const timer = window.setInterval(load, 15_000)
    return () => { controller.abort(); window.clearInterval(timer) }
  }, [])
  const available = useMemo(() => executionGroups.filter(g => groupAccount(dashboard?.accounts ?? [], g)), [dashboard])
  const group = executionGroups.find(g => g.id === requestedGroup) ?? (!requestedGroup && available.length === 1 ? available[0] : undefined)
  const scope = group ? groupAccount(dashboard?.accounts ?? [], group) : null
  const account = scope?.account
  const accounts = scope?.executions
  const groupId = group?.id ?? ''
  // PnL is refreshed explicitly; native execution streams update independently.
  useEffect(() => {
    if (!showPnl || !group || !account || !dashboard) return
    const controller = new AbortController()
    setPnlLoading(true); setPnlError(null); setPnl(null)
    const end = Math.floor(dashboard.generated_at_us / 1000)
    const starts = dashboard.report.sources.filter(s => s.source_id === account.source_id)
      .map(s => account.strategy_pnl_start_ts_us ?? s.initial_position_snapshot_ts_us ?? s.first_fill_ts_us).filter((v): v is number => v != null)
    const start = range === 'all' ? (starts.length ? Math.min(...starts) / 1000 : end) : end - Number(range) * 86_400_000
    let retryTimer: ReturnType<typeof window.setTimeout> | undefined
    const load = () => getTimeline({ sourceIds: [account.source_id], startMs: Math.floor(start), endMs: end, includeTheoretical: false, maxPoints: 500, signal: controller.signal })
      .then(value => {
        if (controller.signal.aborted) return
        requireGroupSources(value.report.selected_source_ids, group.sourceIds)
        setPnl(value); setPnlError(null); setPnlLoading(false)
        if (value.estimated_funding?.loading) retryTimer = window.setTimeout(() => void load(), 10_000)
      })
      .catch(reason => {
        if (controller.signal.aborted) return
        if (reason instanceof ApiError && reason.navPricing?.loading) {
          retryTimer = window.setTimeout(() => void load(), 10_000)
        } else { setPnlError(String(reason)); setPnlLoading(false) }
      })
    void load()
    return () => { controller.abort(); window.clearTimeout(retryTimer) }
  }, [showPnl, range, groupId, revision])
  const funding = pnl?.estimated_funding?.income_quote
  const total = pnl && funding != null ? pnl.report.summary.nav_change_after_fee_quote + funding : null
  return <AppShell active="exec-viz" title="执行监控" subtitle={account?.account ?? '账户执行监控'} icon={Monitor} wide>
    <PageIntro eyebrow="执行监控" title={account?.account ?? '执行监控'} description="统一查看两所的目标仓位、挂单与执行进度。"
      actions={account && <div className="flex flex-wrap gap-4 text-sm"><a href={routes.account(account.source_id)} className="text-brand">账户配置</a><a href={routes.nav(account.source_id)} className="text-brand">账户净值</a><a href={routes.configBindings(account.source_id)} className="text-brand">策略启用</a></div>} />
    {error && <Alert tone="error" className="mb-4">监控入口读取失败：{error}</Alert>}
    {!dashboard ? <p className="py-12 text-center text-muted">正在加载账户执行监控…</p> : !accounts || !account ?
      <Alert tone="warning">所选账户不存在，或没有完整查看权限。</Alert> : <>
      <UnifiedExecutionMonitor key={account.source_id} executions={accounts} />
      <section className="mt-6 rounded-2xl border border-border bg-surface p-5">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <Button variant="ghost" onClick={() => setShowPnl(v => !v)} aria-expanded={showPnl}>组合策略盈亏 · {showPnl ? '收起' : '展开'}</Button>
          {showPnl && <div className="flex items-center gap-2"><Select aria-label="组合盈亏区间" value={range} onChange={e => setRange(e.target.value)}>
            <option value="1">1天</option><option value="7">7天</option><option value="30">30天</option><option value="all">全部</option>
          </Select><Button size="sm" disabled={pnlLoading} onClick={() => setRevision(n => n + 1)}><RefreshCw size={13} />刷新</Button></div>}
        </div>
        {showPnl && <div className="mt-4 space-y-4">
          {pnlLoading && <p className="flex items-center gap-2 text-sm text-muted"><LoaderCircle size={14} className="animate-spin" />正在计算两侧策略盈亏…</p>}
          {pnlError && <Alert tone="error">{pnlError}</Alert>}
          {pnl && <>
            <div className="grid gap-3 md:grid-cols-3"><StatTile label="合计费后交易盈亏" value={money(pnl.report.summary.nav_change_after_fee_quote)} />
              <StatTile label="预估 Funding" value={funding != null ? fundingMoney(funding) : pnl.estimated_funding?.loading ? '加载中…' : '--'} />
              <StatTile label="合计盈亏（含预估 Funding）" value={total != null ? money(total) : '--'} /></div>
            {pnl.estimated_funding?.unavailable_reason && <Alert tone="warning">{pnl.estimated_funding?.unavailable_reason}</Alert>}
            <p className="text-xs text-muted">{timestampUs(pnl.report.start_ts_us)} — {timestampUs(pnl.report.end_ts_us)} · 两侧分别核算后相加 <Badge tone="neutral">Funding 预估</Badge></p>
            {pnl.report.strategy_points.length === 0 ? <p className="py-4 text-center text-sm text-muted">当前区间暂无策略记录。</p> :
              <div className="overflow-x-auto"><table className="w-full text-sm"><thead><tr className="border-b border-border text-left text-muted"><th className="p-2">策略</th><th className="p-2">费后交易盈亏</th><th className="p-2">预估 Funding</th><th className="p-2">合计</th></tr></thead><tbody>
                {pnl.report.strategy_points.map(strategy => {
                  const fee = pnl.estimated_funding?.strategies.find(s => s.strategy === strategy.strategy)?.income_quote
                  const combined = fee != null ? strategy.summary.nav_change_after_fee_quote + fee : null
                  return <tr key={strategy.strategy} className="border-b border-border-soft"><td className="p-2">{strategyLabel(strategy.strategy)}</td>
                    <td className={cn('p-2 tabular-nums', signedClass(strategy.summary.nav_change_after_fee_quote))}>{money(strategy.summary.nav_change_after_fee_quote)}</td>
                    <td className="p-2 tabular-nums">{fee != null ? fundingMoney(fee) : '--'}</td><td className="p-2 tabular-nums">{combined != null ? money(combined) : '--'}</td></tr>
                })}
              </tbody></table></div>}
          </>}
        </div>}
      </section>
    </>}
  </AppShell>
}
