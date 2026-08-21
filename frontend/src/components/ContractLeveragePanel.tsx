import type { ReactNode } from 'react'
import { Button } from './ui/Button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from './ui/Card'
import { Input } from './ui/Field'

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
        <p className="text-xs text-muted sm:col-span-4">
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
          按单个合约调用交易所 setLeverage，只调整交易所保证金设置，不参与 CTA 目标数量计算。
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4 pt-0">
        {toolbar}
        <div className="rounded-xl border border-border-soft bg-canvas/60 px-4 py-3 text-sm leading-relaxed text-muted">
          范围 1–125。查询读取交易所实时值，本地记录只展示上次请求值。
        </div>
      </CardContent>
    </Card>
  )
}
