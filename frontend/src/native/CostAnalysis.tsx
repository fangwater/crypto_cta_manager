import { useEffect, useState } from 'react'
import { Alert } from '../components/ui/Badge'
import { Button } from '../components/ui/Button'
import { Card, CardContent } from '../components/ui/Card'
import { Input } from '../components/ui/Field'
import { getCostReport, type CostReport } from './api'

const number = (v: number | null) => v === null ? '—' : v.toLocaleString('zh-CN', { maximumFractionDigits: 6 })
const offsets: Record<string, string> = { open: '开仓', close: '平仓', close_today: '平今', close_yesterday: '平昨' }
const direction = (side: string, offset: string) => `${side === 'buy' ? '买' : '卖'}／${offsets[offset] ?? offset}`

export default function CostAnalysis({ sourceId, tradingDay }: { sourceId: string; tradingDay?: string | null }) {
  const [day, setDay] = useState(tradingDay ?? '')
  const [report, setReport] = useState<CostReport | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)
  const [refresh, setRefresh] = useState(0)
  const [page, setPage] = useState(0)
  useEffect(() => { if (tradingDay) setDay(current => current || tradingDay) }, [tradingDay])
  useEffect(() => {
    setReport(null); setError(null); setPage(0)
    if (!sourceId || !/^\d{8}$/.test(day)) { setLoading(false); return }
    const controller = new AbortController()
    setLoading(true)
    getCostReport(sourceId, day, controller.signal).then(data => {
      if (!controller.signal.aborted) setReport(data)
    }).catch(e => {
      if (!controller.signal.aborted) setError(e instanceof Error ? e.message : String(e))
    }).finally(() => { if (!controller.signal.aborted) setLoading(false) })
    return () => controller.abort()
  }, [sourceId, day, refresh])
  const metrics: [string, number | null][] = report ? [
    ['全部成交价差成本（元）', report.price_shortfall_cny],
    ['全部成交手续费差（元）', report.fee_shortfall_cny],
    ['全部成交总成本差（元）', report.total_shortfall_cny],
    ['已定价成交价差小计（元）', report.covered_price_shortfall_cny],
  ] : []
  return <div className="space-y-4">
    <div className="flex flex-wrap items-center gap-3">
      <label className="flex items-center gap-2 text-sm">交易日<Input aria-label="成本分析交易日" value={day} placeholder="YYYYMMDD" inputMode="numeric" maxLength={8} onChange={e => setDay(e.target.value)} /></label>
      <Button disabled={loading || !sourceId || !/^\d{8}$/.test(day)} onClick={() => setRefresh(v => v + 1)}>重新计算</Button>
      {loading && <span role="status" className="text-sm text-muted">正在计算…</span>}
    </div>
    <p className="text-sm text-muted">按真实成交手数比较理论执行价格与手续费，正值表示实际成本更高。手续费按合约和开平规则计算，开仓、平仓、平今、平昨分别维护。交易日及夜盘归属以维护的数据为准；分钟参考价不包含盘口价差与市场冲击。</p>
    {error && <Alert tone="error">{error}</Alert>}
    {!/^\d{8}$/.test(day) && <p className="text-sm text-muted">请输入需要复盘的交易日。</p>}
    {report && <>
      {report.replication && <p className="text-sm text-muted">本次复盘使用本地同步副本 · 源视图 {report.replication.source_captured_at_us === null ? '—' : new Date(report.replication.source_captured_at_us / 1000).toLocaleString('zh-CN', { timeZone: 'Asia/Shanghai', hour12: false })}{report.replication.last_error ? ` · ${report.replication.last_error}` : ''}</p>}
      {report.reason && <Alert tone="warning">{report.reason}</Alert>}
      {report.market_data_error && <Alert tone="warning">{report.market_data_error}</Alert>}
      {!report.complete && <Alert tone="warning">参考数据或成交对照尚不完整。缺少覆盖的整体指标显示为“—”，已定价小计仅包含具备参考价格的成交。</Alert>}
      <div className="native-stats">{metrics.map(([name, value]) => <Card key={name}><CardContent className="py-4"><p className="text-sm text-muted">{name}</p><p className="mt-1 text-xl font-semibold tabular-nums">{number(value)}</p></CardContent></Card>)}</div>
      <p className="text-sm text-muted">价格覆盖 {report.priced_fill_count}／{report.fill_count} 笔、{report.priced_lots}／{report.total_lots} 手 · 手续费对照 {report.fee_compared_fill_count} 笔</p>
      {report.provenance && <p className="break-words text-sm text-muted">数据来源：{report.provenance} · 参考修订 {report.reference_sha256?.slice(0, 12)}</p>}
      <h2 className="font-semibold">理论执行参考</h2>
      <p className="text-sm text-muted">窗口从信号之后首个完整交易分钟开始，按已维护的时段跳过休市，限于同一交易日。分钟均价＝成交额÷成交手数÷合约乘数；TWAP 对分钟均价等权，VWAP 按成交手数加权。</p>
      <div className="native-table-scroll"><table className="native-table"><thead><tr>{['参考编号', '交易所／合约', '方向／开平', '窗口', '参考手数／已定价手数', '理论成交价', '数据状态'].map(h => <th key={h}>{h}</th>)}</tr></thead><tbody>
        {report.signals.map(s => <tr key={s.signal_id}><td>{s.signal_id}</td><td>{s.venue}／{s.symbol}</td><td>{direction(s.side, s.offset)}</td><td>{s.window_minutes} 分钟 {s.model.toUpperCase()}</td><td>{s.expected_lots}／{s.priced_lots}</td><td>{number(s.theoretical_price)}</td><td>{s.reason ?? '价格完整'}</td></tr>)}
      </tbody></table></div>
      <h2 className="font-semibold">真实成交对照</h2>
      <div className="native-table-scroll"><table className="native-table"><thead><tr>{['交易所／合约', '成交号', '会话／引用', '方向／开平', '手数', '参考编号', '成交价／理论价', '实际手续费', '理论手续费（元）', '价差成本（元）', '手续费差（元）', '总成本差（元）', '数据状态'].map(h => <th key={h}>{h}</th>)}</tr></thead><tbody>
        {report.fills.slice(page * 100, (page + 1) * 100).map(f => <tr key={`${f.venue}:${f.symbol}:${f.trade_id}:${f.side}`}><td>{f.venue}／{f.symbol}</td><td>{f.trade_id}</td><td>{f.session ?? '—'}／{f.order_ref ?? '—'}</td><td>{direction(f.side, f.offset)}</td><td>{f.lots}</td><td>{f.signal_id ?? '—'}</td><td>{number(f.actual_price)}／{number(f.theoretical_price)}</td><td>{f.actual_commission === null ? '未报告' : `${number(f.actual_commission)} ${f.commission_currency ?? '币种未报告'}`}</td><td>{number(f.theoretical_commission_cny)}</td><td>{number(f.price_shortfall_cny)}</td><td>{number(f.fee_shortfall_cny)}</td><td>{number(f.total_shortfall_cny)}</td><td>{f.reason ?? '对照完整'}</td></tr>)}
      </tbody></table></div>
      {report.fills.length > 100 && <div className="flex items-center gap-3"><Button disabled={page === 0} onClick={() => setPage(v => v - 1)}>上一页</Button><span className="text-sm">{page + 1}／{Math.ceil(report.fills.length / 100)}</span><Button disabled={(page + 1) * 100 >= report.fills.length} onClick={() => setPage(v => v + 1)}>下一页</Button></div>}
    </>}
  </div>
}
