import { useEffect, useRef, useState } from 'react'
import * as echarts from 'echarts/core'
import { LineChart } from 'echarts/charts'
import { DataZoomComponent, GridComponent, LegendComponent, TooltipComponent } from 'echarts/components'
import { CanvasRenderer } from 'echarts/renderers'
import { Alert } from '../components/ui/Badge'
import { Button } from '../components/ui/Button'
import { Card, CardContent } from '../components/ui/Card'
import { Input } from '../components/ui/Field'
import * as api from './api'
import { ApiError } from '../api'

echarts.use([LineChart, DataZoomComponent, GridComponent, LegendComponent, TooltipComponent, CanvasRenderer])
const time = (us: number | null | undefined) => us == null ? '—' : new Date(us / 1000).toLocaleString('zh-CN', { timeZone: 'Asia/Shanghai', hour12: false })
const number = (value: number | null | undefined) => value == null ? '—' : value.toLocaleString('zh-CN', { maximumFractionDigits: 6 })
const inputTime = (ms: number) => new Date(ms + 8 * 3600_000).toISOString().slice(0, 23)
const parseTime = (value: string) => value ? Date.parse(`${value}+08:00`) : NaN
const message = (e: unknown) => e instanceof Error ? e.message : String(e)

export default function NavAnalysis({ sourceId, snapshot, canConfigure }: { sourceId: string; snapshot: api.Snapshot | null; canConfigure: boolean }) {
  const [anchors, setAnchors] = useState<api.NavAnchor[] | null>(null)
  const [start, setStart] = useState(''), [end, setEnd] = useState('')
  const [query, setQuery] = useState<{ start: number; end: number; revision: number } | null>(null)
  const [report, setReport] = useState<api.NavReport | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(false), [saving, setSaving] = useState(false)
  const [waitingOnSync, setWaitingOnSync] = useState(false)
  const [note, setNote] = useState('原生持仓起始快照')
  const latest = anchors?.[0], through = snapshot?.nav_anchor_observed_us
  useEffect(() => {
    if (!sourceId) return
    const controller = new AbortController()
    api.getNavAnchors(sourceId, controller.signal).then(setAnchors).catch(e => { if (!controller.signal.aborted) setError(message(e)) })
    return () => controller.abort()
  }, [sourceId])
  useEffect(() => {
    if (!latest || !through || start || end) return
    const begin = Math.ceil(latest.snapshot_us / 1000), finish = Math.floor(through / 1000)
    setStart(inputTime(begin)); setEnd(inputTime(finish))
    if (finish >= begin && finish - begin <= 31 * 86400_000) setQuery({ start: begin, end: finish, revision: 0 })
  }, [latest, through, start, end])
  useEffect(() => {
    setReport(null)
    if (!query || !sourceId) return
    const controller = new AbortController()
    let timer: ReturnType<typeof setTimeout> | undefined
    setLoading(true); setError(null); setWaitingOnSync(false)
    const load = async () => {
      let retry = false
      try {
        const data = await api.getNativeNav(sourceId, query.start, query.end, controller.signal)
        if (!controller.signal.aborted) { setReport(data); setWaitingOnSync(false) }
      } catch (e) {
        if (!controller.signal.aborted) {
          if (e instanceof ApiError && e.status === 503) {
            retry = true; setWaitingOnSync(true); timer = setTimeout(() => void load(), 2000)
          } else { setError(message(e)); setWaitingOnSync(false) }
        }
      } finally {
        if (!controller.signal.aborted && !retry) setLoading(false)
      }
    }
    void load()
    return () => { controller.abort(); clearTimeout(timer) }
  }, [sourceId, query])
  const calculate = () => {
    const begin = parseTime(start), finish = parseTime(end)
    if (!Number.isFinite(begin) || !Number.isFinite(finish) || begin > finish || finish - begin > 31 * 86400_000) { setError('请选择最多 31 天的有效范围，时间为上海时间'); return }
    setQuery(old => ({ start: begin, end: finish, revision: (old?.revision ?? 0) + 1 }))
  }
  const range = (days: number) => {
    if (!through || !latest) return
    const finish = Math.floor(through / 1000), begin = Math.max(Math.ceil(latest.snapshot_us / 1000), finish - days * 86400_000)
    setStart(inputTime(begin)); setEnd(inputTime(finish)); setQuery(old => ({ start: begin, end: finish, revision: (old?.revision ?? 0) + 1 }))
  }
  const capture = async () => {
    if (!snapshot?.nav_anchor_observed_us) return
    setSaving(true); setError(null)
    try { await api.captureNavAnchor(sourceId, snapshot.nav_anchor_observed_us, note); setAnchors(await api.getNavAnchors(sourceId)); setStart(''); setEnd(''); setQuery(null); setReport(null) }
    catch (e) { setError(message(e)) } finally { setSaving(false) }
  }
  return <div className="space-y-4">
    <p className="text-sm text-muted">净值显示所选时段的人民币盈亏变化。按多空、今昨仓及真实开平成交 FIFO 回放，持仓使用已完成分钟收盘价计价。资金出入、结算划转和账户账单费用不在本曲线内。</p>
    {error && <Alert tone="error">{error}</Alert>}
    {waitingOnSync && <p className="text-sm text-muted">账户数据正在更新，稍后自动重试…</p>}
    <Card><CardContent className="space-y-3 py-4"><p className="text-sm">最新起始快照：{latest ? `${time(latest.snapshot_us)} · 交易日 ${latest.trading_day} · ${latest.positions.length} 条多空持仓` : anchors === null ? '正在读取…' : '尚未建立'}</p>
      {canConfigure && <><p className="text-sm text-muted">将 {time(snapshot?.nav_anchor_observed_us)} 的事实持仓及分钟参考价保存为以后重算的固定起点，已有快照保留。执行状态：{snapshot?.status ?? '未报告'}。</p><div className="flex flex-wrap items-center gap-3"><Input aria-label="净值快照说明" value={note} onChange={e => setNote(e.target.value)} /><Button disabled={saving || !sourceId || !snapshot?.nav_anchor_observed_us || snapshot.projection_errors > 0 || !note.trim()} onClick={() => void capture()}>{saving ? '保存中…' : '建立起始快照'}</Button></div></>}
    </CardContent></Card>
    <div className="flex flex-wrap items-center gap-3"><label className="text-sm">开始（上海）<Input type="datetime-local" step="0.001" value={start} onChange={e => setStart(e.target.value)} /></label><label className="text-sm">结束（上海）<Input type="datetime-local" step="0.001" value={end} onChange={e => setEnd(e.target.value)} /></label><Button disabled={loading || !latest || !start || !end} onClick={calculate}>{loading ? '计算中…' : '计算净值'}</Button>{[1, 7, 30].map(days => <Button key={days} disabled={loading || !latest || !through} onClick={() => range(days)}>{days}D</Button>)}</div>
    <p className="text-sm text-muted">完整账户观察截至 {time(through)}。程序启动或心跳不会延长账户事实的覆盖时间。</p>
    {report?.reason && <Alert tone="warning">{report.reason}</Alert>}
    {report?.ready && report.summary && <>
      <p className="text-sm text-muted">本次结果：{time(report.start_us)} 至 {time(report.end_us)} · 执行证据截至 {time(report.source_through_us)}</p>
      <div className="native-stats">{([['费用后净值变化（元）', report.summary.nav_after_fees_cny], ['费用前盈亏（元）', report.summary.nav_before_fees_cny], ['已实现盈亏（元）', report.summary.realized_cny], ['浮动变化（元）', report.summary.floating_cny], ['交易手续费（元）', report.summary.fees_cny]] as [string, number | null][]).map(([name, value]) => <Card key={name}><CardContent className="py-4"><p className="text-sm text-muted">{name}</p><p className="mt-1 text-xl font-semibold tabular-nums">{number(value)}</p></CardContent></Card>)}</div>
      <p className="text-sm text-muted">{report.fees.fill_count} 笔成交：实际 CNY 扣费 {report.fees.actual_count} 笔、按当时账户费率估算 {report.fees.estimated_count} 笔、费用未知 {report.fees.missing_count} 笔。按合约及开仓、平仓、平今、平昨规则计费。</p>
      <NavChart report={report} />
      <div className="native-table-scroll"><table className="native-table"><thead><tr>{['交易所／合约', '多仓（手）', '空仓（手）', '已实现（元）', '浮动变化（元）', '费用前盈亏（元）'].map(h => <th key={h}>{h}</th>)}</tr></thead><tbody>{report.markets.map(m => <tr key={`${m.venue}:${m.symbol}`}><td>{m.venue}／{m.symbol}</td><td>{m.long_lots}</td><td>{m.short_lots}</td><td>{number(m.realized_cny)}</td><td>{number(m.floating_cny)}</td><td>{number(m.nav_before_fees_cny)}</td></tr>)}</tbody></table></div>
      <p className="text-sm text-muted">历史成交尚无明确策略归属，当前展示账户及合约层级盈亏。</p>
    </>}
  </div>
}
function NavChart({ report }: { report: api.NavReport }) {
  const element = useRef<HTMLDivElement>(null)
  useEffect(() => {
    if (!element.current) return
    const chart = echarts.init(element.current)
    const series: [string, keyof api.NavPoint][] = [['费用后净值', 'nav_after_fees_cny'], ['费用前盈亏', 'nav_before_fees_cny'], ['已实现盈亏', 'realized_cny'], ['浮动变化', 'floating_cny'], ['手续费', 'fees_cny']]
    chart.setOption({ animation: false, tooltip: { trigger: 'axis' }, legend: { type: 'scroll' }, grid: { left: 76, right: 24, top: 48, bottom: 78 }, xAxis: { type: 'time', axisLabel: { formatter: (v: number) => new Date(v).toLocaleString('zh-CN', { timeZone: 'Asia/Shanghai', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', hour12: false }) } }, yAxis: { type: 'value', scale: true, name: 'CNY' }, dataZoom: [{ type: 'inside' }, { type: 'slider', bottom: 8 }], series: series.map(([name, key]) => ({ name, type: 'line', showSymbol: false, connectNulls: false, data: report.points.map(p => [p.timestamp_us / 1000, p[key]]) })) })
    const observer = new ResizeObserver(() => chart.resize()); observer.observe(element.current)
    return () => { observer.disconnect(); chart.dispose() }
  }, [report])
  return <div ref={element} role="img" aria-label="人民币净值、已实现及浮动盈亏曲线" style={{ height: 420, width: '100%', minWidth: 0 }} />
}
