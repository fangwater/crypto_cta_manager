import { Timer } from 'lucide-react'
import { FieldHint, Input, Label, Select } from './ui/Field'
import {
  ORDER_PARAMETER_FIELDS,
  makerPriceAnchorOptions,
  orderParameterMeta,
} from '../lib/orderParametersMeta'
import type { ChaseParameters, OrderParameters, PovParameters } from '../types'
import { formatDuration, maxEstimatedExecutionMs } from '../lib/executionTiming'

const algorithmOptions = [
  { value: 'batch', label: 'Batch' },
  { value: 'pov', label: 'POV' },
  { value: 'chase', label: 'Chase' },
] as const

const povFields = [
  ['participation_rate', '参与率', '0.01', '0.000001'],
  ['max_batch_usdt', '单次释放上限 (USDT)', '1', '0.01'],
  ['max_carry_usdt', '累计额度上限 (USDT)', '1', '0.01'],
  ['volume_stale_ms', '成交量有效期 (ms)', '1', '1'],
  ['quote_stale_ms', '盘口有效期 (ms)', '1', '1'],
  ['duration_ms', '执行期限 (ms)', '1000', '1'],
] as const satisfies ReadonlyArray<[keyof PovParameters, string, string, string]>

const chaseFields = [
  ['batch_floor_usdt', '最小批次名义金额 (USDT)', '1', '0.01'],
  ['max_batch', '最大批次数', '1', '1'],
  ['max_open_batches', '最大在途批数', '1', '1'],
  ['maker_recenter_trigger_bps', '追价触发 (bps)', '0.1', '0'],
  ['maker_amend_cooldown_ms', '改单冷却 (ms)', '1', '0'],
  ['maker_timeout_sec', 'Maker 超时 (s)', '1', '1'],
  ['target_tolerance_usdt', '目标容差 (USDT)', '1', '0'],
  ['strategy_order_rate_limit_per_min', 'Chase 策略 60 秒报单上限', '1', '0'],
  ['strategy_order_rate_limit_10s', 'Chase 策略 10 秒报单上限', '1', '0'],
] as const satisfies ReadonlyArray<[keyof ChaseParameters, string, string, string]>

export function OrderParametersForm({
  value,
  onChange,
}: {
  value: OrderParameters
  onChange: (value: OrderParameters) => void
}) {
  const updatePov = (field: keyof PovParameters, next: number | string | null) => {
    onChange({ ...value, pov: { ...value.pov, [field]: next } })
  }
  const updateChase = (field: keyof ChaseParameters, next: number) => {
    onChange({ ...value, chase: { ...value.chase, [field]: next } })
  }

  return (
    <div className="grid gap-5">
      <Label>
        执行算法
        <Select
          value={value.algorithm}
          onChange={(event) =>
            onChange({
              ...value,
              algorithm: event.target.value as OrderParameters['algorithm'],
            })
          }
        >
          {algorithmOptions.map((option) => (
            <option key={option.value} value={option.value}>
              {option.label}
            </option>
          ))}
        </Select>
        <FieldHint>
          Batch 按批次推进；POV 按公开成交量释放；Chase 在己方一档挂单并原地改单追价。
        </FieldHint>
      </Label>

      <label className="flex cursor-pointer items-start gap-3 border-l-2 border-brand px-3 py-2">
        <input
          type="checkbox"
          className="mt-1 accent-brand"
          checked={value.signal_execution_enabled}
          onChange={(event) =>
            onChange({ ...value, signal_execution_enabled: event.target.checked })
          }
        />
        <span>
          <span className="block text-sm font-medium text-ink">启用 signal 执行语义</span>
          <span className="mt-1 block text-xs leading-5 text-muted">
            开启时向 Exec 透传 signal；关闭时归档保留原值，但写入 Exec Redis 前置为 0。下一次仓位发布或手动重发时生效。
          </span>
        </span>
      </label>

      {value.algorithm !== 'chase' ? (
        <>
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
                      onChange({ ...value, [field]: Number(event.target.value) })
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
                  maker_price_anchor: event.target
                    .value as OrderParameters['maker_price_anchor'],
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
                makerPriceAnchorOptions.find(
                  (option) => option.value === value.maker_price_anchor,
                )?.hint
              }
            </FieldHint>
          </Label>
        </>
      ) : (
        <div className="grid gap-4 sm:grid-cols-2">
          {chaseFields.map(([field, label, step, min]) => (
            <Label key={field}>
              {label}
              <Input
                type="number"
                step={step}
                min={min}
                value={value.chase[field]}
                onChange={(event) => updateChase(field, Number(event.target.value))}
              />
            </Label>
          ))}
          <FieldHint className="sm:col-span-2">
            以实际 Chase 策略名汇总全部 symbol 的新单和改单；0 关闭对应窗口，并与账户 Exec 限频叠加。
          </FieldHint>
        </div>
      )}

      {value.algorithm === 'pov' && (
        <section className="border-t border-border pt-5">
          <div className="mb-4 text-sm font-semibold text-ink">POV 参数</div>
          <div className="grid gap-4 sm:grid-cols-2">
            {povFields.map(([field, label, step, min]) => (
              <Label key={field}>
                {label}
                <Input
                  type="number"
                  step={step}
                  min={min}
                  value={String(value.pov[field])}
                  onChange={(event) => updatePov(field, Number(event.target.value))}
                />
              </Label>
            ))}
            <Label>
              流动性模式
              <Select
                value={value.pov.liquidity}
                onChange={(event) => updatePov('liquidity', event.target.value)}
              >
                <option value="maker_only">Maker Only</option>
                <option value="maker_then_taker">Maker Then Taker</option>
                <option value="taker_only">Taker Only</option>
              </Select>
            </Label>
            <Label>
              限价
              <Input
                type="number"
                step="0.00000001"
                min="0"
                value={value.pov.limit_price ?? ''}
                placeholder="不限制"
                onChange={(event) =>
                  updatePov(
                    'limit_price',
                    event.target.value.trim() === '' ? null : Number(event.target.value),
                  )
                }
              />
              <FieldHint>仅 Maker Only 可设置；买入为价格上限，卖出为价格下限。</FieldHint>
            </Label>
          </div>
        </section>
      )}

      {value.algorithm !== 'chase' && (
        <div className="flex items-start gap-3 border-l-2 border-brand px-3 py-2 text-sm text-muted">
          <Timer className="mt-0.5 shrink-0 text-brand" size={17} />
          <div>
            <div className="font-medium text-ink">
              {value.algorithm === 'pov'
                ? `POV 最长执行 ${formatDuration(value.pov.duration_ms)}`
                : `最大预估执行时间 ${formatDuration(maxEstimatedExecutionMs(value))}`}
            </div>
            <div className="mt-1 text-xs leading-5">
              {value.algorithm === 'pov'
                ? '到期后撤销工作单并保留未完成目标，不会强制扫单。'
                : `按 ${value.max_batch || 0} 批、每批间隔 ${value.batch_interval_ms || 0} ms，以及每批最多 ${Math.max(1, (value.max_maker_requotes || 0) + 1)} 轮 maker 等待估算。`}
            </div>
          </div>
        </div>
      )}
    </div>
  )
}
