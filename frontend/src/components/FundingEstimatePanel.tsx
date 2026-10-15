import type { FundingEstimate } from '../types'
import { fundingMoney, timestampUs } from '../format'
import { AnalysisMetric } from './AnalysisMetric'

export function FundingEstimatePanel({ value }: { value?: FundingEstimate }) {
  if (!value) return null
  return <section className="chart-section" aria-label="估算资金费">
    <div className="panel-heading"><h2>估算资金费</h2><span>{value.loading ? '加载中' : value.income_quote != null ? '已计算' : '暂不可用'}</span></div>
    {value.income_quote != null && <div className="summary-strip nav-analysis-stats">
      <AnalysisMetric label="区间估算 Funding" value={fundingMoney(value.income_quote)} detail="正数收入，负数支出" />
      {Object.entries(value.income_by_asset).map(([asset, amount]) => <AnalysisMetric key={asset} label={asset} value={fundingMoney(amount)} />)}
      {value.reconciliation && <AnalysisMetric label="共同覆盖范围的账单差异" value={value.reconciliation.difference_quote.toFixed(8)} detail={`已核对 ${value.reconciliation.actual_record_count} 笔，${value.reconciliation.mismatch_count} 次结算超出误差阈值`} />}
    </div>}
    <p className="chart-notice">按各策略结算整点前持仓及公开历史资金费率、结算标记价格估算，无需开启账户资金费采集。每 30 分钟刷新。{value.synced_through_ts_us != null && `费率已查询至 ${timestampUs(value.synced_through_ts_us)}。`}</p>
    {Boolean(value.reconciliation?.unobserved_settlements) && <p className="chart-notice">另有 {value.reconciliation?.unobserved_settlements} 次持仓结算尚无完整账单覆盖，未纳入差异比较。</p>}
    <p className="chart-notice">RapidX 的结算仓位截点和标记价格取样可能与公开历史记录不同，估算金额可能与实际账单有差异。</p>
    {value.unavailable_reason && <p className="chart-notice" role="status">{value.unavailable_reason}</p>}
  </section>
}
