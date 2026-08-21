import {
  makerPriceAnchorOptions,
  orderParameterMeta,
} from '../lib/orderParametersMeta'
import type { OrderParameters } from '../types'

export function OrderParametersView({ value }: { value: OrderParameters }) {
  const anchorLabel =
    makerPriceAnchorOptions.find((option) => option.value === value.maker_price_anchor)?.label ??
    value.maker_price_anchor

  const rows = [
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
