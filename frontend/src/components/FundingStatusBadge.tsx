import type { FundingSummary } from '../types'
import { Badge } from './ui/Badge'

export const fundingStatusText: Record<string, string> = {
  loading: '加载中', waiting_for_symbols: '等待订单', backfilling: '历史补齐中',
  ok: '正常', error: '采集异常', disabled: '已关闭',
}

export function FundingStatusBadge({ value }: { value?: FundingSummary }) {
  const active = value?.enabled ?? false
  const status = value?.status ?? 'disabled'
  return <Badge tone={!active ? 'neutral' : status === 'ok' ? 'success' : 'warning'}>
    Funding {active ? '已开启' : '已关闭'}{active && ` · ${fundingStatusText[status] ?? status}`}
  </Badge>
}
