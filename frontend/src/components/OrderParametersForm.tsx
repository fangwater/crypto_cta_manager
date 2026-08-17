import { FieldHint, Input, Label, Select } from './ui/Field'
import {
  ORDER_PARAMETER_FIELDS,
  makerPriceAnchorOptions,
  orderParameterMeta,
} from '../lib/orderParametersMeta'
import type { OrderParameters } from '../types'

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
    </div>
  )
}
