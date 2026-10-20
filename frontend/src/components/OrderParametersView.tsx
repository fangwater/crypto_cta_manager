import {
  makerPriceAnchorOptions,
  orderParameterMeta,
} from '../lib/orderParametersMeta'
import type { OrderParameters } from '../types'

export function OrderParametersView({ value }: { value: OrderParameters }) {
  if (value.algorithm === 'grid') {
    return <ParameterRows rows={[
      { label: '算法', value: 'Grid · Vol 网格' },
      { label: 'Signal 执行', value: value.signal_execution_enabled ? '启用' : '关闭' },
      { label: '单手量', value: `${value.single_order_usdt} 报价币` },
      { label: '拆单数', value: String(value.orders_per_batch) },
      { label: '批次间隔', value: `${value.batch_interval_ms} ms` },
      { label: 'Maker 挂单时长', value: `${value.maker_timeout_ms} ms` },
      { label: '现货 Maker 挂单次数', value: String(value.max_maker_requotes + 1) },
      { label: '合约 Maker 挂单次数', value: '不限' },
      { label: '目标容差', value: `${value.target_tolerance_usdt} 报价币` },
      { label: '买侧 Vol 倍数', value: value.grid.open_buy_vol_scale.join(' → ') },
      { label: '卖侧 Vol 倍数', value: value.grid.open_sell_vol_scale.join(' → ') },
      { label: '最小内档偏移', value: String(value.grid.open_offset_lower) },
      { label: 'Vol 有效期', value: `${value.grid.vol_stale_ms} ms` },
      { label: '盘口有效期', value: `${value.grid.quote_stale_ms} ms` },
    ]} />
  }
  if (value.algorithm === 'ctaker') {
    return <ParameterRows rows={[{ label: '算法', value: 'CTaker' }, { label: '执行', value: '全部仓位差额直接市价；按原 qty 平仓' }]} />
  }
  if (value.algorithm === 'chase') {
    const chaseRows = [
      { label: '算法', value: 'Chase' },
      { label: 'Signal 执行', value: value.signal_execution_enabled ? '启用' : '关闭' },
      { label: '最小批次名义金额', value: `${value.chase.batch_floor_usdt} 报价币` },
      { label: '最大批次数', value: String(value.chase.max_batch) },
      { label: '最大在途批数', value: String(value.chase.max_open_batches) },
      { label: '追价触发', value: `${value.chase.maker_recenter_trigger_bps} bps` },
      { label: '改单冷却', value: `${value.chase.maker_amend_cooldown_ms} ms` },
      { label: 'Maker 超时', value: `${value.chase.maker_timeout_sec} s` },
      { label: '目标容差', value: `${value.chase.target_tolerance_usdt} 报价币` },
      {
        label: '策略 60 秒报单上限',
        value: value.chase.strategy_order_rate_limit_per_min === 0
          ? '关闭'
          : String(value.chase.strategy_order_rate_limit_per_min),
      },
      {
        label: '策略 10 秒报单上限',
        value: value.chase.strategy_order_rate_limit_10s === 0
          ? '关闭'
          : String(value.chase.strategy_order_rate_limit_10s),
      },
    ]
    return <ParameterRows rows={chaseRows} />
  }

  const anchorLabel =
    makerPriceAnchorOptions.find((option) => option.value === value.maker_price_anchor)?.label ??
    value.maker_price_anchor

  const rows = [
    { label: '算法', value: value.algorithm === 'pov' ? 'POV' : 'Batch' },
    { label: 'Signal 执行', value: value.signal_execution_enabled ? '启用' : '关闭' },
    { label: orderParameterMeta.single_order_usdt.label, value: `${value.single_order_usdt} 报价币` },
    { label: orderParameterMeta.orders_per_batch.label, value: String(value.orders_per_batch) },
    { label: orderParameterMeta.max_batch.label, value: String(value.max_batch) },
    { label: orderParameterMeta.maker_price_anchor.label, value: anchorLabel },
    { label: orderParameterMeta.tick_spacing.label, value: String(value.tick_spacing) },
    { label: orderParameterMeta.batch_interval_ms.label, value: `${value.batch_interval_ms} ms` },
    { label: orderParameterMeta.maker_timeout_ms.label, value: `${value.maker_timeout_ms} ms` },
    { label: orderParameterMeta.max_maker_requotes.label, value: String(value.max_maker_requotes) },
    {
      label: orderParameterMeta.target_tolerance_usdt.label,
      value: `${value.target_tolerance_usdt} 报价币`,
    },
  ]

  if (value.algorithm === 'pov') {
    rows.push(
      { label: '参与率', value: String(value.pov.participation_rate) },
      { label: '单次释放上限', value: `${value.pov.max_batch_usdt} 报价币` },
      { label: '累计额度上限', value: `${value.pov.max_carry_usdt} 报价币` },
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
