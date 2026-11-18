import { useEffect, useRef, useState, type ReactNode } from 'react'
import { getNativeContractRates, type NativeContractRateLookup } from '../api'
import { timestampUs } from '../format'
import { Button } from './ui/Button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from './ui/Card'
import { Input } from './ui/Field'

export function ContractLeverageToolbar({
  symbol,
  contractLeverage,
  queriedLeverage,
  saving,
  readOnly = false,
  native = false,
  queryResult,
  symbolPlaceholder = 'BTCUSDT / BTCUSDC',
  onSymbolChange,
  onContractLeverageChange,
  onQuery,
  onSave,
}: {
  symbol: string
  contractLeverage: string
  queriedLeverage?: string | null
  saving?: boolean
  readOnly?: boolean
  native?: boolean
  queryResult?: ReactNode
  symbolPlaceholder?: string
  onSymbolChange: (value: string) => void
  onContractLeverageChange?: (value: string) => void
  onQuery: () => void
  onSave?: () => void
}) {
  return (
    <form
      className="grid max-w-2xl items-end gap-3 sm:grid-cols-[9rem_7.5rem_auto_auto]"
      onSubmit={(event) => {
        event.preventDefault()
        if (!readOnly) onSave?.()
      }}
    >
      <label className="grid gap-1.5 text-xs font-medium text-muted">
        合约
        <Input
          value={symbol}
          placeholder={symbolPlaceholder}
          onChange={(event) => onSymbolChange(native ? event.target.value : event.target.value.toUpperCase())}
        />
      </label>
      <label className="grid gap-1.5 text-xs font-medium text-muted">
        {native ? '多空保证金率' : '合约杠杆'}
        <Input
          value={contractLeverage}
          inputMode="numeric"
          disabled={readOnly}
          onChange={(event) => onContractLeverageChange?.(event.target.value)}
        />
      </label>
      <Button type="button" variant="secondary" disabled={saving} onClick={onQuery}>
        查询
      </Button>
      <Button type="submit" variant="primary" disabled={saving || readOnly}>
        设置
      </Button>
      {queryResult}
      {queriedLeverage ? (
        <p className="text-xs text-muted sm:col-span-4">
          交易所当前杠杆 <span className="font-medium text-ink">{queriedLeverage}x</span>
        </p>
      ) : null}
    </form>
  )
}

export function ContractLeveragePanel({ toolbar, native = false }: { toolbar?: ReactNode; native?: boolean }) {
  return (
    <Card className="overflow-hidden">
      <CardHeader>
        <CardTitle>{native ? '合约保证金' : '交易所合约杠杆'}</CardTitle>
        <CardDescription>
          {native ? '查询选定账户和合约的柜台保证金率及每手金额。' : '按单个合约调用交易所 setLeverage，只调整交易所保证金设置，不参与 CTA 目标数量计算。'}
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4 pt-0">
        {toolbar}
        <div className="rounded-xl border border-border-soft bg-canvas/60 px-4 py-3 text-sm leading-relaxed text-muted">
          {native ? '显示最近一次已采集的柜台观察。保证金由柜台设置。' : '范围 1–125。查询读取交易所实时值，本地记录只展示上次请求值。'}
        </div>
      </CardContent>
    </Card>
  )
}

export function isNativeFuturesVenue(venue?: string) {
  return venue === 'ctp' || venue === 'yida'
}

const percent = new Intl.NumberFormat('zh-CN', { style: 'percent', maximumFractionDigits: 16 })
const perLotAmount = new Intl.NumberFormat('zh-CN', { maximumFractionDigits: 16 })

export function formatNativeContractRate(rate: number, perLot: number) {
  return `${percent.format(rate)} + ${perLotAmount.format(perLot)} CNY/手`
}

export const nativeFeeFields = [
  ['open', '开仓'],
  ['close', '平仓（含平昨）'],
  ['close_today', '平今'],
] as const

export function NativeContractRatesPanel({ sourceId, disabled, onRates }: {
  sourceId: string
  disabled?: boolean
  onRates?: (value: NativeContractRateLookup | null) => void
}) {
  const [symbol, setSymbol] = useState('')
  const [result, setResult] = useState<NativeContractRateLookup | null>(null)
  const [error, setError] = useState('')
  const [loading, setLoading] = useState(false)
  const request = useRef<AbortController | null>(null)
  useEffect(() => () => request.current?.abort(), [sourceId])

  async function query() {
    request.current?.abort()
    const controller = new AbortController()
    request.current = controller
    setResult(null)
    onRates?.(null)
    setError('')
    setLoading(true)
    try {
      const next = await getNativeContractRates(sourceId, symbol, controller.signal)
      if (controller.signal.aborted) return
      setResult(next)
      onRates?.(next)
    } catch (reason) {
      if (!controller.signal.aborted) setError(reason instanceof Error ? reason.message : String(reason))
    } finally {
      if (!controller.signal.aborted) setLoading(false)
    }
  }

  const rates = result?.rates
  return <ContractLeveragePanel native toolbar={
    <ContractLeverageToolbar
      native readOnly symbol={symbol} symbolPlaceholder="SHFE:cu2612"
      contractLeverage={rates ? `${percent.format(rates.long_margin_rate)} / ${percent.format(rates.short_margin_rate)}` : ''}
      saving={loading || disabled}
      onSymbolChange={value => {
        request.current?.abort()
        setLoading(false)
        setSymbol(value)
        setResult(null)
        onRates?.(null)
        setError('')
      }}
      onQuery={() => void query()}
      queryResult={error ? <p role="alert" className="text-xs text-rose-700 sm:col-span-4">{error}</p>
        : rates ? <div role="status" className="space-y-1 text-xs text-muted sm:col-span-4">
          <p>{rates.venue}:{rates.symbol} · 多头 {formatNativeContractRate(rates.long_margin_rate, rates.long_margin_per_lot)} · 空头 {formatNativeContractRate(rates.short_margin_rate, rates.short_margin_per_lot)}</p>
          {!onRates && <p>{nativeFeeFields.map(([field, label]) =>
            `${label} ${formatNativeContractRate(rates.fees[field].notional_rate, rates.fees[field].per_lot)}`
          ).join(' · ')}</p>}
          <p>交易日 {rates.trading_day} · 观察于 {timestampUs(rates.observed_at_us)}</p>
        </div> : result ? <p role="status" className="text-xs text-muted sm:col-span-4">该合约暂无柜台费率观察。</p> : null}
    />
  } />
}
