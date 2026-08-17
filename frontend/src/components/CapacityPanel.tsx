import { money } from '../format'
import { cn } from '../lib/cn'
import type { AccountCapacity } from '../types'
import { Badge } from './ui/Badge'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from './ui/Card'

function sharesText(value: number | null | undefined) {
  if (value === null || value === undefined || !Number.isFinite(value)) return '--'
  return value.toFixed(2)
}

export function CapacityPanel({
  capacity,
  compact = false,
}: {
  capacity?: AccountCapacity | null
  compact?: boolean
}) {
  const live = capacity?.live
  const status = live?.status ?? 'unavailable'
  const statusLabel =
    status === 'ok' ? '实时' : status === 'stale' ? '延迟' : '未接入 account monitor'
  const remaining = capacity?.remaining_shares

  return (
    <Card className="overflow-hidden">
      <CardHeader className="flex flex-row items-start justify-between gap-3">
        <div>
          <CardTitle>权益与可配置份数</CardTitle>
          <CardDescription>
            一份 = {capacity ? money(capacity.share_unit_usdt) : '10,000'} USDT 参考权益。可用名义 =
            实时权益 × 杠杆率。
          </CardDescription>
        </div>
        <Badge tone={status === 'ok' ? 'success' : status === 'stale' ? 'warning' : 'neutral'}>
          {statusLabel}
        </Badge>
      </CardHeader>
      <CardContent className="space-y-4 pt-0">
        <div className={cn('grid gap-3', compact ? 'sm:grid-cols-2' : 'sm:grid-cols-2 xl:grid-cols-4')}>
          <Metric
            label="实时权益"
            value={live ? `${money(live.equity_usdt)} USDT` : '--'}
            hint={
              live
                ? `钱包 ${money(live.wallet_balance_usdt)} + 浮盈 ${money(live.unrealized_pnl_usdt)}`
                : '订阅 account_monitor IPC'
            }
          />
          <Metric
            label="杠杆率"
            value={capacity ? `${capacity.leverage}x` : '--'}
            hint="账户愿意把实时权益放大到多少名义"
          />
          <Metric
            label="可用名义"
            value={
              capacity?.buying_power_usdt != null
                ? `${money(capacity.buying_power_usdt)} USDT`
                : '--'
            }
            hint="实时权益 × 杠杆率"
          />
          <Metric
            label="可配置份数"
            value={sharesText(capacity?.configurable_shares)}
            hint={`已启用 ${sharesText(capacity?.bound_shares)} 份 · 剩余 ${sharesText(remaining)} 份`}
            tone={
              remaining != null && remaining < 0
                ? 'text-rose-700'
                : remaining != null && remaining < 0.5
                  ? 'text-amber-700'
                  : undefined
            }
          />
        </div>
        <div className="rounded-xl border border-border-soft bg-canvas/60 px-4 py-3 text-sm leading-relaxed text-muted">
          <p className="font-medium text-ink">杠杆率是什么？</p>
          <p className="mt-1">
            这里的杠杆不是交易所保证金杠杆，也不是当前持仓名义 / 权益。它是账户级的
            <strong className="font-medium text-ink"> CTA 配置倍数</strong>
            ：Manager 用「实时权益 × 杠杆率」得到这本账能配置的总名义，再除以单份参考权益得到份数。
          </p>
          <p className="mt-2">
            例如权益 25,000 USDT、杠杆 2x、单份 10,000 USDT，则可配置 5.00 份。已启用两条各 1
            份的仓位策略时，剩余 3.00 份。
          </p>
        </div>
      </CardContent>
    </Card>
  )
}

function Metric({
  label,
  value,
  hint,
  tone,
}: {
  label: string
  value: string
  hint?: string
  tone?: string
}) {
  return (
    <div className="rounded-xl border border-border-soft bg-surface/80 px-3 py-3">
      <p className="text-[11px] text-subtle">{label}</p>
      <p className={cn('mt-1 text-sm font-semibold tabular-nums text-ink', tone)}>{value}</p>
      {hint && <p className="mt-1 text-[11px] leading-relaxed text-subtle">{hint}</p>}
    </div>
  )
}
