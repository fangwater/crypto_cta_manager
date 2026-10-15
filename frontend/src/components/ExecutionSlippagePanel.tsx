import { AnalysisMetric } from './AnalysisMetric'
import type { ExecutionSlippage } from '../types'
import { money, timestampUs } from '../format'

export function ExecutionSlippagePanel({ value }: { value?: ExecutionSlippage }) {
  if (!value) return null
  const bps = (n: number | null) => n == null ? '--' : `${n.toFixed(2)} bps`
  return <section className="chart-section" aria-label="净值差滑点">
    <div className="panel-heading"><h2>净值差滑点</h2></div>
    <div className="summary-strip nav-analysis-stats">
      <AnalysisMetric label="费前滑点" value={bps(value.before_fee_bps)} detail={`净值差 ${value.before_fee_gap_quote == null ? '--' : money(value.before_fee_gap_quote)} 报价币`} />
      <AnalysisMetric label="费后滑点" value={bps(value.after_fee_bps)} detail={`净值差 ${value.after_fee_gap_quote == null ? '--' : money(value.after_fee_gap_quote)} 报价币`} />
      <AnalysisMetric label="比较区间实际成交额" value={money(value.volume_quote)} detail="报价币" />
    </div>
    <p className="chart-notice">（理论净值 − 实际净值）÷ 同期实际成交额 × 10,000；正值表示实际表现更差，funding 不计入滑点。{value.end_ts_us != null && `比较截至 ${timestampUs(value.end_ts_us)}。`}</p>
    <p className="chart-notice">净值差包含仓位与估值差异；同成交量的价格与费用滑点见“成本分析”。</p>
    {value.unavailable_reason && <p className="chart-notice" role="status">{value.unavailable_reason}</p>}
  </section>
}
