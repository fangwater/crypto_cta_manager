import { Timer } from 'lucide-react'
import { FieldHint, Input, Label, Select } from './ui/Field'
import {
  ORDER_PARAMETER_FIELDS,
  makerPriceAnchorOptions,
  orderParameterMeta,
} from '../lib/orderParametersMeta'
import type { OrderParameters } from '../types'
import { formatDuration, maxEstimatedExecutionMs } from '../lib/executionTiming'

export function OrderParametersForm({
  value,
  onChange,
}: {
  value: OrderParameters
  onChange: (value: OrderParameters) => void
}) {
  return (
    <div className="grid gap-4">
      <div className="grid gap-4 sm:grid-cols-2">
        {ORDER_PARAMETER_FIELDS.map((field) => {
          const meta = orderParameterMeta[field]
          return (
            <Label key={field}>
              {meta.label}
              <Input
                type="number"
                step={meta.step}
                min={meta.min}
                value={value[field]}
                onChange={(event) =>
                  onChange({
                    ...value,
                    [field]: Number(event.target.value),
                  })
                }
              />
              <FieldHint>{meta.hint}</FieldHint>
            </Label>
          )
        })}
      </div>
      <Label>
        {orderParameterMeta.maker_price_anchor.label}
        <Select
          value={value.maker_price_anchor}
          onChange={(event) =>
            onChange({
              ...value,
              maker_price_anchor: event.target.value as OrderParameters['maker_price_anchor'],
            })
          }
        >
          {makerPriceAnchorOptions.map((option) => (
            <option key={option.value} value={option.value}>
              {option.label}
            </option>
          ))}
        </Select>
        <FieldHint>
          {orderParameterMeta.maker_price_anchor.hint}{' '}
          {
            makerPriceAnchorOptions.find((option) => option.value === value.maker_price_anchor)
              ?.hint
          }
        </FieldHint>
      </Label>
      <div className="flex items-start gap-3 border-l-2 border-brand px-3 py-2 text-sm text-muted">
        <Timer className="mt-0.5 shrink-0 text-brand" size={17} />
        <div>
          <div className="font-medium text-ink">
            最大预估执行时间 {formatDuration(maxEstimatedExecutionMs(value))}
          </div>
          <div className="mt-1 text-xs leading-5">
            按 {value.max_batch || 0} 批、每批间隔 {value.batch_interval_ms || 0} ms，以及
            每批最多 {Math.max(1, (value.max_maker_requotes || 0) + 1)} 轮 maker 等待估算；不含行情等待、撤单确认和网络延迟。
          </div>
        </div>
      </div>
    </div>
  )
}
