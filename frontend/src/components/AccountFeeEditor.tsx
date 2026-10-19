import { useEffect, useState } from 'react'
import type { AccountStudio, QuoteFeeRates } from '../types'
import { feeBps } from '../format'
import { Button } from './ui/Button'
import { FieldHint, Input, Label } from './ui/Field'

export function AccountFeeEditor({ studio, disabled, onSave }: {
  studio: AccountStudio
  disabled: boolean
  onSave: (maker: number, taker: number, theory: number, quotes: Record<string, QuoteFeeRates>) => void
}) {
  const [quote, setQuote] = useState(studio.quote_fee_rates.USDC ? 'USDC' : '')
  const [separate, setSeparate] = useState(false)
  const [values, setValues] = useState(['', '', ''])
  const [error, setError] = useState('')
  useEffect(() => {
    const override = studio.quote_fee_rates[quote]
    const rates = override ?? studio
    setSeparate(Boolean(override))
    setValues([rates.maker_fee_rate, rates.taker_fee_rate, rates.theoretical_twap_fee_rate].map(String))
    setError('')
  }, [studio, quote])
  const save = () => {
    const numbers = values.map(Number)
    if (values.some(value => !value.trim()) || numbers.some(value => !Number.isFinite(value))) {
      setError('费率必须是有限数字，负数表示返佣')
      return
    }
    const [maker, taker, theory] = numbers
    const quotes = { ...studio.quote_fee_rates }
    if (quote) {
      if (separate) quotes[quote] = { maker_fee_rate: maker, taker_fee_rate: taker, theoretical_twap_fee_rate: theory }
      else delete quotes[quote]
      onSave(studio.maker_fee_rate, studio.taker_fee_rate, studio.theoretical_twap_fee_rate, quotes)
    } else onSave(maker, taker, theory, quotes)
  }
  return <div className="space-y-3">
    <div className="flex flex-wrap items-center gap-3">
      <Label>适用报价币
        <select className="rounded border border-border-soft bg-white px-3 py-2" value={quote}
          disabled={disabled} onChange={event => setQuote(event.target.value)}>
          <option value="">默认费率</option>
          {['USDT', 'USDC', 'USD'].map(asset => <option key={asset} value={asset}>{asset}</option>)}
        </select>
      </Label>
      {quote && <label className="flex items-center gap-2 text-sm">
        <input type="checkbox" checked={separate} disabled={disabled}
          onChange={event => {
            setSeparate(event.target.checked)
            if (!event.target.checked) setValues([studio.maker_fee_rate, studio.taker_fee_rate, studio.theoretical_twap_fee_rate].map(String))
          }} />单独设置 {quote} 费率
      </label>}
    </div>
    <div className="flex flex-wrap items-end gap-3">
      {['Maker', 'Taker', '理论 TWAP'].map((label, index) => <Label key={label} className="min-w-[12rem] flex-1">
        {label} 费率（小数）
        <Input inputMode="decimal" value={values[index]} disabled={disabled || Boolean(quote && !separate)}
          onChange={event => setValues(previous => previous.map((value, i) => i === index ? event.target.value : value))} />
      </Label>)}
      <Button type="button" size="sm" disabled={disabled} onClick={save}>保存</Button>
    </div>
    {error && <p role="alert" className="text-sm text-rose-700">{error}</p>}
    <FieldHint>负数表示返佣。理论 TWAP 可设为 Maker/Taker 的平均值；已归档目标保留发布时的费率。</FieldHint>
    <div className="space-y-1 text-xs text-muted">
      <p>默认：Maker {feeBps(studio.maker_fee_rate)} / Taker {feeBps(studio.taker_fee_rate)} / 理论 {feeBps(studio.theoretical_twap_fee_rate)}</p>
      {Object.entries(studio.quote_fee_rates).map(([asset, rates]) => <p key={asset}>
        {asset}：Maker {feeBps(rates.maker_fee_rate)} / Taker {feeBps(rates.taker_fee_rate)} / 理论 {feeBps(rates.theoretical_twap_fee_rate)}
      </p>)}
    </div>
  </div>
}
