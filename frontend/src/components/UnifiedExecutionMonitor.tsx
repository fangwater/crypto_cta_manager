import { ChevronDown, ChevronRight, Radio, RefreshCw, Search } from 'lucide-react'
import { Fragment, useMemo, useState } from 'react'
import type { DashboardAccount } from '../types'
import { useExecMonitor } from '../hooks/useExecMonitor'
import { combinedExecRisk, execFeedLabel, executionView, type ExecDisplayRow, type ExecFilters, type ExecGroup } from '../lib/execMonitor'
import { money, quantity, signedClass, strategyLabel } from '../format'
import { cn } from '../lib/cn'
import { Badge } from './ui/Badge'
import { Button } from './ui/Button'
import { Card } from './ui/Card'
import { Input, Select } from './ui/Field'

const exchange = (venue: string) => venue.startsWith('okex') ? 'OKX' : 'Binance'
const marketLabel = (venue: string) => `${exchange(venue)}${venue.endsWith('margin') ? ' · 现货' : ' · 合约'}`
const q = (v: number | null) => v == null ? '—' : quantity(v)
const usd = (v: number | null) => v == null ? '—' : money(v)
const time = (v: number | null) => v == null || v <= 0 ? '—' : new Date(v).toLocaleTimeString('zh-CN', { hour12: false })
const cell = 'whitespace-nowrap px-4 py-3 text-right tabular-nums'

function Values({ target, current, live, pending, delta, liveUsdt, pendingUsdt, batches, eta }: {
  target: number | null; current: number | null; live: number | null; pending: number | null
  delta: number | null; liveUsdt: number | null; pendingUsdt: number | null; batches: number | null; eta: number | null
}) {
  return <><td className={cn(cell, signedClass(target ?? 0))}>{q(target)}</td><td className={cn(cell, signedClass(current ?? 0))}>{q(current)}</td>
    <td className={cell}>{q(live)}</td><td className={cell}>{q(pending)}</td><td className={cn(cell, signedClass(delta ?? 0))}>{usd(delta)}</td>
    <td className={cell}>{usd(liveUsdt)}</td><td className={cell}>{usd(pendingUsdt)}</td><td className={cell}>{batches == null ? '—' : batches}</td>
    <td className={cell} title={eta ? new Date(eta).toLocaleString('zh-CN') : undefined}>{time(eta)}</td></>
}
function DetailRow({ entry, groupBy }: { entry: ExecDisplayRow; groupBy: ExecFilters['groupBy'] }) {
  const { row, status, feed } = entry
  const allocated = feed.state?.ready && row.position_allocated
  return <tr className="border-b border-border-soft text-xs text-muted hover:bg-canvas/50" data-execution-row={row.key} data-source={row.sourceId} data-symbol={row.symbol}>
    <th scope="row" className="max-w-64 whitespace-nowrap py-3 pl-10 pr-4 text-left font-normal" title={row.strategy_name}>
      <span className="block max-w-60 truncate">{groupBy === 'symbol' ? strategyLabel(row.strategy_name) : row.symbol}</span>
      {groupBy === 'symbol' && <span className="mt-1 block text-[10px] text-subtle">更新 {time(row.source_updated_at_ms)}</span>}
    </th><td className="whitespace-nowrap px-4 py-3">{marketLabel(row.venue)}</td><td className="px-4 py-3"><Badge tone={status.tone} title={status.detail}>{status.label}</Badge></td>
    <Values target={row.target_qty} current={allocated ? row.current_qty : null} live={row.live_order_qty} pending={allocated ? row.pending_qty : null}
      delta={allocated ? row.delta_usdt : null} liveUsdt={row.live_order_usdt == null ? null : Math.abs(row.live_order_usdt)}
      pendingUsdt={!allocated || row.pending_usdt == null ? null : Math.abs(row.pending_usdt)} batches={row.remaining_batches} eta={row.execution_complete ? null : row.estimated_completion_ts_ms} />
  </tr>
}
function GroupRow({ group, open, toggle, groupBy }: { group: ExecGroup; open: boolean; toggle: () => void; groupBy: ExecFilters['groupBy'] }) {
  return <Fragment>
    <tr className="border-b border-border bg-canvas/65 text-xs font-medium text-ink" data-execution-group={group.key}>
      <th scope="row" className="max-w-72 px-4 py-3 text-left"><button type="button" className="flex w-full items-center gap-2 text-left" onClick={toggle} aria-expanded={open} aria-label={`${open ? '收起' : '展开'} ${group.label} ${group.venue ? exchange(group.venue) : ''} 明细`}>
        {open ? <ChevronDown size={14} className="shrink-0 text-muted" /> : <ChevronRight size={14} className="shrink-0 text-muted" />}
        <span className="min-w-0"><span className="block truncate" title={group.label}>{groupBy === 'strategy' ? strategyLabel(group.label) : group.label}</span><span className="mt-1 block text-[10px] font-normal text-muted">{group.detail}</span></span>
      </button></th>
      <td className="whitespace-nowrap px-4 py-3">{group.venue ? <Badge tone={group.venue.startsWith('okex') ? 'brand' : 'neutral'}>{marketLabel(group.venue)}</Badge> : '跨市场合计'}</td>
      <td className="px-4 py-3"><Badge tone={group.status.tone} title={group.status.detail}>{group.status.label}</Badge></td>
      <Values target={group.target} current={group.current} live={group.live} pending={group.pending} delta={group.deltaUsdt}
        liveUsdt={group.liveUsdt} pendingUsdt={group.pendingUsdt} batches={group.batches} eta={group.eta} />
    </tr>
    {open && group.children.map(entry => <DetailRow key={entry.row.key} entry={entry} groupBy={groupBy} />)}
  </Fragment>
}

export function UnifiedExecutionMonitor({ executions }: { executions: DashboardAccount[] }) {
  const { feeds, now, refresh } = useExecMonitor(executions)
  const [filters, setFilters] = useState<ExecFilters>({ strategy: '', symbol: '', sourceId: '', groupBy: 'symbol' })
  const [expanded, setExpanded] = useState<Set<string>>(new Set())
  const risk = useMemo(() => combinedExecRisk(feeds, now), [feeds, now])
  const view = useMemo(() => executionView(feeds, filters, now), [feeds, filters, now])
  const readyCount = feeds.filter(f => execFeedLabel(f, now) === '实时').length
  const filter = (next: Partial<ExecFilters>) => setFilters(current => ({ ...current, ...next }))
  const metrics = [
    ['实时权益', usd(risk.equity_usdt), 'USDT'], ['组合杠杆', risk.leverage == null ? '—' : risk.leverage.toFixed(3), '倍'],
    ['总名义价值', usd(risk.gross_notional_usdt), 'USDT'], ['净名义价值', usd(risk.net_notional_usdt), 'USDT'],
    ['多头名义价值', usd(risk.long_notional_usdt), 'USDT'], ['空头名义价值', usd(risk.short_notional_usdt), 'USDT'],
  ]
  return <div className="min-w-0 space-y-5" data-unified-execution-monitor>
    <div className="flex flex-wrap items-center justify-between gap-3 text-xs">
      <div className="flex flex-wrap items-center gap-x-5 gap-y-2" aria-label="执行数据连接">
        {feeds.map(feed => {
          const label = execFeedLabel(feed, now), live = label === '实时'
          return <span key={feed.source.sourceId} className="inline-flex items-center gap-2" data-execution-connection={feed.source.sourceId}>
            <span className={cn('h-2 w-2 rounded-full', live ? 'bg-emerald-500' : feed.connection === 'live' && !feed.error ? 'bg-amber-400' : 'bg-rose-400')} />
            <strong className="font-medium text-ink">{exchange(feed.source.venue)}</strong><span className="text-muted">{label}</span>
            {feed.state && <time className="tabular-nums text-subtle" title={new Date(feed.state.tsMs).toLocaleString('zh-CN')}>{time(feed.state.tsMs)}</time>}
          </span>
        })}
      </div><Button size="sm" onClick={refresh} aria-label="刷新执行数据"><RefreshCw size={13} />刷新</Button>
    </div>
    <Card className="overflow-hidden"><div className="grid grid-cols-2 divide-border lg:grid-cols-6" aria-label="账户执行汇总">
      {metrics.map(([label, value, unit]) => <div key={label} className="border-b border-r border-border-soft px-5 py-5 last:border-r-0 lg:border-b-0" data-exec-metric={label}>
        <p className="text-xs text-muted">{label}</p><p className="mt-2 text-xl font-semibold tabular-nums tracking-tight text-ink">{value}<span className="ml-1.5 text-[10px] font-normal text-subtle">{unit}</span></p>
      </div>)}
    </div></Card>
    <Card className="min-w-0 overflow-hidden" data-execution-table-panel>
      <div className="flex flex-wrap items-center justify-between gap-4 border-b border-border px-5 py-4">
        <div><h3 className="text-base font-semibold text-ink">执行仓位</h3><p className="mt-1 text-xs text-muted">{readyCount}/{feeds.length} 个交易所数据就绪 · {view.strategies.length} 个策略</p></div>
        <div className="flex rounded-lg bg-canvas p-1" role="group" aria-label="执行分组">
          <Button size="sm" variant={filters.groupBy === 'symbol' ? 'primary' : 'ghost'} aria-pressed={filters.groupBy === 'symbol'} onClick={() => filter({ groupBy: 'symbol' })}>按品种</Button>
          <Button size="sm" variant={filters.groupBy === 'strategy' ? 'primary' : 'ghost'} aria-pressed={filters.groupBy === 'strategy'} onClick={() => filter({ groupBy: 'strategy' })}>按策略</Button>
        </div>
      </div>
      <div className="grid gap-3 border-b border-border-soft px-5 py-4 sm:grid-cols-3" aria-label="执行筛选">
        <Select aria-label="交易所筛选" value={filters.sourceId} onChange={e => filter({ sourceId: e.target.value })}><option value="">全部交易所</option>{executions.map(a => <option key={a.source_id} value={a.source_id}>{exchange(a.venue)}</option>)}</Select>
        <Select aria-label="策略筛选" value={filters.strategy} onChange={e => filter({ strategy: e.target.value })}><option value="">全部策略</option>{[...new Set([...view.strategies, ...(filters.strategy ? [filters.strategy] : [])])].map(name => <option key={name} value={name}>{strategyLabel(name)}</option>)}</Select>
        <label className="relative"><Search size={14} className="pointer-events-none absolute left-3 top-3 text-subtle" /><Input className="pl-9" aria-label="品种筛选" placeholder="筛选品种，如 ETH" value={filters.symbol} onChange={e => filter({ symbol: e.target.value })} /></label>
      </div>
      <div className="flex flex-wrap items-center justify-between gap-3 border-b border-border-soft bg-canvas/50 px-5 py-3 text-xs" aria-label="执行金额汇总">
        <span className="text-muted">{view.complete ? '当前筛选 · 金额按绝对值合计' : '等待完整执行数据，合计暂不可用'}</span>
        <div className="flex flex-wrap gap-x-6 gap-y-2 text-muted">{[['执行差额', view.summary.deltaUsdt], ['挂单金额', view.summary.liveUsdt], ['待下单金额', view.summary.pendingUsdt]].map(([label, value]) => <span key={String(label)}>{label} <strong className="ml-1 font-semibold tabular-nums text-ink" data-work-metric={label}>{usd(value as number | null)}</strong> USDT</span>)}</div>
      </div>
      <div className="overflow-x-auto"><table className="w-full min-w-[1180px] border-collapse text-xs" aria-label="统一执行仓位">
        <thead className="border-b border-border text-[11px] text-muted"><tr>
          <th className="px-4 py-3 text-left font-medium">{filters.groupBy === 'symbol' ? '品种 / 策略' : '策略 / 品种'}</th><th className="px-4 py-3 text-left font-medium">交易所</th><th className="px-4 py-3 text-left font-medium">状态</th>
          {['目标数量', '实际数量', '挂单数量', '待下单数量', '差额 USDT', '挂单 USDT', '待下单 USDT', '剩余批次', '预计完成'].map(label => <th key={label} className="whitespace-nowrap px-4 py-3 text-right font-medium" title={label === '实际数量' ? '按品种汇总时使用该交易所账户仓位；策略明细使用策略仓位。不同市场数量不相加。' : label.includes('USDT') ? '挂单、待下单及跨市场策略合计使用绝对金额，避免相反方向抵消。' : undefined}>{label}</th>)}
        </tr></thead><tbody>{view.groups.map(group => <GroupRow key={`${filters.groupBy}:${group.key}`} group={group} groupBy={filters.groupBy} open={expanded.has(`${filters.groupBy}:${group.key}`)} toggle={() => setExpanded(current => {
          const next = new Set(current), key = `${filters.groupBy}:${group.key}`; if (next.has(key)) next.delete(key); else next.add(key); return next
        })} />)}</tbody>
      </table></div>
      {!view.groups.length && <div className="flex flex-col items-center gap-3 px-5 py-16 text-center" role="status"><Radio size={28} strokeWidth={1.3} className="text-subtle" />
        <p className="font-medium text-ink">{feeds.every(f => !f.state) ? '等待执行仓位数据' : view.complete ? '当前筛选下没有执行任务' : '执行数据暂未就绪'}</p>
        <p className="max-w-md text-xs leading-relaxed text-muted">{feeds.every(f => !f.state) ? '收到执行状态后，两所的目标仓位、挂单和执行进度会显示在同一张表中。' : '可调整交易所、策略或品种筛选。'}</p>
      </div>}
    </Card>
  </div>
}
