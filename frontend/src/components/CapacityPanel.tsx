import type { ReactNode } from 'react'
import { money } from '../format'
import { cn } from '../lib/cn'
import type { AccountCapacity } from '../types'
import { Badge } from './ui/Badge'
import { Button } from './ui/Button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from './ui/Card'
import { Input } from './ui/Field'

export function CapacityPanel({
  capacity,
  compact = false,
  toolbar,
}: {
  capacity?: AccountCapacity | null
  compact?: boolean
  toolbar?: ReactNode
}) {
  const live = capacity?.live
  const status = live?.status ?? 'unavailable'
  const statusLabel =
    status === 'ok' ? '实时' : status === 'stale' ? '延迟' : '未接入 account monitor'
  const remaining = capacity?.remaining_notional_usdt

  return (
    <Card className="overflow-hidden">
      <CardHeader className="flex flex-row items-start justify-between gap-3">
        <div>
          <CardTitle>权益与可配置名义</CardTitle>
          <CardDescription>
            已配置名义 = Σ(份数 × 该策略单份参考权益) × 杠杆。各策略参考权益可以不同，按名义金额聚合，不按统一份数折算。
          </CardDescription>
        </div>
        <Badge tone={status === 'ok' ? 'success' : status === 'stale' ? 'warning' : 'neutral'}>
          {statusLabel}
        </Badge>
      </CardHeader>
      <CardContent className="space-y-4 pt-0">
        {toolbar}
        <div className={cn('grid gap-3', compact ? 'grid-cols-2' : 'grid-cols-2 lg:grid-cols-4')}>
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
            hint="CTA 配置倍数；发布仓位 = 模板 qty × 份数 × 杠杆"
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
            label="已配置名义"
            value={
              capacity ? `${money(capacity.bound_notional_usdt)} USDT` : '--'
            }
            hint={
              remaining != null
                ? `剩余 ${money(remaining)} USDT`
                : 'Σ(份数 × 策略参考权益)'
            }
            tone={
              remaining != null && remaining < 0
                ? 'text-rose-700'
                : remaining != null && remaining < 1_000
                  ? 'text-amber-700'
                  : undefined
            }
          />
        </div>
        <div className="rounded-xl border border-border-soft bg-canvas/60 px-4 py-3 text-sm leading-relaxed text-muted">
          <p className="font-medium text-ink">杠杆率是什么？</p>
          <p className="mt-1">
            这里的杠杆不是交易所保证金杠杆。它是账户级的
            <strong className="font-medium text-ink"> CTA 配置倍数</strong>
            ：可用名义 = 实时权益 × 杠杆率；发布到 Exec 的仓位 = 仓位模板 qty × 份数 ×
            杠杆。已配置名义先按各策略自己的参考权益加权求和，再乘杠杆，例如（1 份 × 10,000 + 1 份 ×
            20,000）× 2x = 60,000 USDT，不是统一按 10,000 折成 3 份。
          </p>
          <p className="mt-2">
            例如权益 25,000 USDT、杠杆 2x，可用名义 50,000 USDT；若未加杠杆的绑定名义是 30,000，则已配置名义
            60,000，剩余 -10,000。保存杠杆后会按新倍数重算并推送本账户全部绑定策略。页面和脚本都通过{' '}
            <code className="text-[12px] text-ink">PUT /api/catalog/accounts/&lt;source_id&gt;/leverage</code>{' '}
            修改杠杆。
          </p>
        </div>
      </CardContent>
    </Card>
  )
}

export function LeverageToolbar({
  account,
  leverage,
  saving,
  onLeverageChange,
  onSave,
}: {
  account?: ReactNode
  leverage: string
  saving?: boolean
  onLeverageChange: (value: string) => void
  onSave: () => void
}) {
  return (
    <form
      className={cn(
        'grid items-end gap-3',
        account ? 'grid-cols-[minmax(0,1fr)_7.5rem_auto]' : 'grid-cols-[7.5rem_auto]',
      )}
      onSubmit={(event) => {
        event.preventDefault()
        onSave()
      }}
    >
      {account}
      <label className="grid gap-1.5 text-xs font-medium text-muted">
        杠杆率
        <Input
          value={leverage}
          inputMode="decimal"
          onChange={(event) => onLeverageChange(event.target.value)}
        />
      </label>
      <Button type="submit" variant="primary" disabled={saving}>
        保存
      </Button>
    </form>
  )
}

export function ContractLeverageToolbar({
  symbol,
  contractLeverage,
  queriedLeverage,
  saving,
  onSymbolChange,
  onContractLeverageChange,
  onQuery,
  onSave,
}: {
  symbol: string
  contractLeverage: string
  queriedLeverage?: string | null
  saving?: boolean
  onSymbolChange: (value: string) => void
  onContractLeverageChange: (value: string) => void
  onQuery: () => void
  onSave: () => void
}) {
  return (
    <form
      className="grid max-w-2xl items-end gap-3 sm:grid-cols-[9rem_7.5rem_auto_auto]"
      onSubmit={(event) => {
        event.preventDefault()
        onSave()
      }}
    >
      <label className="grid gap-1.5 text-xs font-medium text-muted">
        合约
        <Input
          value={symbol}
          placeholder="BTCUSDT"
          onChange={(event) => onSymbolChange(event.target.value.toUpperCase())}
        />
      </label>
      <label className="grid gap-1.5 text-xs font-medium text-muted">
        合约杠杆
        <Input
          value={contractLeverage}
          inputMode="numeric"
          onChange={(event) => onContractLeverageChange(event.target.value)}
        />
      </label>
      <Button type="button" variant="secondary" disabled={saving} onClick={onQuery}>
        查询
      </Button>
      <Button type="submit" variant="primary" disabled={saving}>
        设置
      </Button>
      {queriedLeverage ? (
        <p className="sm:col-span-4 text-xs text-muted">
          交易所当前杠杆 <span className="font-medium text-ink">{queriedLeverage}x</span>
        </p>
      ) : null}
    </form>
  )
}

export function ContractLeveragePanel({ toolbar }: { toolbar?: ReactNode }) {
  return (
    <Card className="overflow-hidden">
      <CardHeader>
        <CardTitle>交易所合约杠杆</CardTitle>
        <CardDescription>
          按单个合约调用交易所 setLeverage。这不会改 CTA 仓位倍数，也不会让 pre-trade 检查或强制重设。
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4 pt-0">
        {toolbar}
        <div className="rounded-xl border border-border-soft bg-canvas/60 px-4 py-3 text-sm leading-relaxed text-muted">
          <p>
            页面和脚本通过{' '}
            <code className="text-[12px] text-ink">
              GET /api/catalog/accounts/&lt;source_id&gt;/contract-leverage?symbol=BTCUSDT
            </code>{' '}
            查询交易所当前杠杆，通过{' '}
            <code className="text-[12px] text-ink">
              PUT /api/catalog/accounts/&lt;source_id&gt;/contract-leverage
            </code>{' '}
            设置，例如 <code className="text-[12px] text-ink">{`{"symbol":"BTCUSDT","contract_leverage":5}`}</code>
            。范围 1–125。查询读交易所实时值，不把本地上次设置当真相。
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
