import { Minus, TrendingDown, TrendingUp } from 'lucide-react'
import { useMemo } from 'react'
import { quantity } from '../format'
import { cn } from '../lib/cn'
import { Badge } from './ui/Badge'

function directionMeta(value: number) {
  if (value > 0) return { label: '多', icon: TrendingUp, valueClass: 'text-emerald-700' }
  if (value < 0) return { label: '空', icon: TrendingDown, valueClass: 'text-rose-700' }
  return { label: '平', icon: Minus, valueClass: 'text-subtle' }
}

export function TargetPositionsView({
  targets,
  compact = false,
}: {
  targets: Record<string, number>
  compact?: boolean
}) {
  const rows = useMemo(
    () =>
      Object.entries(targets)
        .map(([symbol, value]) => ({ symbol, value }))
        .filter(({ value }) => value !== 0)
        .sort((left, right) => left.symbol.localeCompare(right.symbol)),
    [targets],
  )
  const total = Object.keys(targets).length

  if (rows.length === 0) {
    return (
      <div className="rounded-xl border border-dashed border-border-soft px-4 py-6 text-center text-sm text-muted">
        {total === 0 ? '暂无目标仓位' : '无非零目标仓位'}
      </div>
    )
  }

  const visible = compact ? rows.slice(0, 6) : rows

  return (
    <div className="overflow-hidden rounded-xl border border-border-soft bg-surface/70">
      <div className="flex items-center justify-between border-b border-border-soft px-4 py-2.5">
        <p className="text-xs font-medium text-muted">非零目标</p>
        <Badge tone="neutral">{rows.length} / {total}</Badge>
      </div>
      <div className={cn('overflow-auto', compact ? 'max-h-48' : 'max-h-80')}>
        <table className="w-full min-w-[360px] border-collapse text-sm">
          <thead>
            <tr className="border-b border-border-soft text-left text-[11px] uppercase tracking-[0.12em] text-subtle">
              <th className="px-4 py-2 font-medium">品种</th>
              <th className="px-3 py-2 font-medium">方向</th>
              <th className="px-4 py-2 text-right font-medium">数量</th>
            </tr>
          </thead>
          <tbody>
            {visible.map(({ symbol, value }) => {
              const meta = directionMeta(value)
              const Icon = meta.icon
              return (
                <tr key={symbol} className="border-b border-border-soft/70 last:border-0">
                  <td className="px-4 py-2 font-mono text-[13px] text-ink">{symbol}</td>
                  <td className="px-3 py-2">
                    <span className="inline-flex items-center gap-1 text-[11px] text-muted">
                      <Icon size={12} />
                      {meta.label}
                    </span>
                  </td>
                  <td className={cn('px-4 py-2 text-right font-mono tabular-nums text-[13px]', meta.valueClass)}>
                    {quantity(value)}
                  </td>
                </tr>
              )
            })}
          </tbody>
        </table>
      </div>
      {compact && rows.length > visible.length && (
        <p className="border-t border-border-soft px-4 py-2 text-[11px] text-subtle">
          另有 {rows.length - visible.length} 个非零品种未展示
        </p>
      )}
    </div>
  )
}
