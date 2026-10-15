import { AnalysisMetric } from './AnalysisMetric'
import type { FundingTimeline } from '../types'
import { money, timestampUs } from '../format'

import { FundingStatusBadge } from './FundingStatusBadge'

export function FundingPanel({ value }: { value?: FundingTimeline }) {
  if (!value || (!value.summary.enabled && value.summary.start_ts_us == null)) return null
  const summary = value.summary
  return <section className="chart-section" aria-label="账户资金费">
    <div className="panel-heading"><h2>账户资金费与总体盈亏</h2><FundingStatusBadge value={summary} /></div>
    <div className="summary-strip nav-analysis-stats">
      <AnalysisMetric label="区间 funding" value={money(summary.income_quote)} detail={`${summary.record_count} 笔，正数收入，负数支出`} />
      <AnalysisMetric label="费前总体盈亏（含 funding）" value={money(value.total_nav_before_fee_quote)} />
      <AnalysisMetric label="费后总体盈亏（含 funding）" value={money(value.total_nav_after_fee_quote)} />
    </div>
    <p className="chart-notice">订单净值 + 实际 funding。资金费归账户，未分配到 CTA 策略。{summary.start_ts_us != null && `观测起点 ${timestampUs(summary.start_ts_us)}。`}{summary.synced_through_ts_us != null && `已检查至 ${timestampUs(summary.synced_through_ts_us)}。`}</p>
    {summary.enabled && summary.status !== 'ok' && <p className="chart-notice" role="status">资金费尚未完整同步，总体盈亏使用目前已保存的记录。</p>}
    {summary.start_ts_us != null && value.points.length > 0 && value.points[0].ts_us < summary.start_ts_us && <p className="chart-notice">所选区间早于资金费观测起点，较早部分未包含 funding。</p>}
    {summary.last_error && <p className="chart-notice" role="status">{summary.last_error}</p>}
    {!summary.enabled && <p className="chart-notice">采集已暂停，已保存的历史 funding 继续计入总体盈亏。</p>}
  </section>
}
