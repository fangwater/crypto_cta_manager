import {
  makerPriceAnchorOptions,
  orderParameterMeta,
} from '../lib/orderParametersMeta'
import type { OrderParameters } from '../types'

export function OrderParametersView({ value }: { value: OrderParameters }) {
  if (value.algorithm === 'chase') {
    const chaseRows = [
      { label: '算法', value: 'Chase' },
      { label: '单笔名义金额', value: `${value.chase.single_order_usdt} USDT` },
      { label: '最大在途金额', value: `${value.chase.max_open_usdt} USDT` },
      { label: '追价触发', value: `${value.chase.maker_recenter_trigger_bps} bps` },
      { label: '改单冷却', value: `${value.chase.maker_amend_cooldown_ms} ms` },
      { label: 'Maker 超时', value: `${value.chase.maker_timeout_ms} ms` },
      { label: '目标容差', value: `${value.chase.target_tolerance_usdt} USDT` },
      { label: '盘口有效期', value: `${value.chase.bbo_max_age_ms} ms` },
    ]
    return <ParameterRows rows={chaseRows} />
  }

  const anchorLabel =
    makerPriceAnchorOptions.find((option) => option.value === value.maker_price_anchor)?.label ??
    value.maker_price_anchor

  const rows = [
    { label: '算法', value: value.algorithm === 'pov' ? 'POV' : 'Batch' },
    { label: orderParameterMeta.single_order_usdt.label, value: `${value.single_order_usdt} USDT` },
    { label: orderParameterMeta.orders_per_batch.label, value: String(value.orders_per_batch) },
    { label: orderParameterMeta.max_batch.label, value: String(value.max_batch) },
    { label: orderParameterMeta.maker_price_anchor.label, value: anchorLabel },
    { label: orderParameterMeta.tick_spacing.label, value: String(value.tick_spacing) },
    { label: orderParameterMeta.batch_interval_ms.label, value: `${value.batch_interval_ms} ms` },
    { label: orderParameterMeta.maker_timeout_ms.label, value: `${value.maker_timeout_ms} ms` },
    { label: orderParameterMeta.max_maker_requotes.label, value: String(value.max_maker_requotes) },
    {
      label: orderParameterMeta.target_tolerance_usdt.label,
      value: `${value.target_tolerance_usdt} USDT`,
    },
  ]

  if (value.algorithm === 'pov') {
    rows.push(
      { label: '参与率', value: String(value.pov.participation_rate) },
      { label: '单次释放上限', value: `${value.pov.max_batch_usdt} USDT` },
      { label: '累计额度上限', value: `${value.pov.max_carry_usdt} USDT` },
      { label: '流动性模式', value: value.pov.liquidity },
      { label: '执行期限', value: `${value.pov.duration_ms} ms` },
    )
  }

  return <ParameterRows rows={rows} />
}

function ParameterRows({ rows }: { rows: Array<{ label: string; value: string }> }) {
  return (
    <dl className="grid gap-3 sm:grid-cols-2">
      {rows.map((row) => (
        <div
          key={row.label}
          className="rounded-xl border border-border-soft bg-canvas/50 px-3 py-3"
        >
          <dt className="text-[11px] font-medium uppercase tracking-[0.08em] text-subtle">
            {row.label}
          </dt>
          <dd className="mt-1 text-sm font-medium tabular-nums text-ink">{row.value}</dd>
        </div>
      ))}
    </dl>
  )
}
