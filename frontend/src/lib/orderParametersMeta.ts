import type { OrderParameters } from '../types'

export const DEFAULT_ORDER_STRATEGY_NAME = 'default_order'

export const ORDER_PARAMETER_FIELDS = [
  'single_order_usdt',
  'orders_per_batch',
  'max_batch',
  'tick_spacing',
  'batch_interval_ms',
  'maker_timeout_ms',
  'max_maker_requotes',
  'target_tolerance_usdt',
] as const satisfies ReadonlyArray<keyof OrderParameters>

type NumericOrderField = (typeof ORDER_PARAMETER_FIELDS)[number]

export const orderParameterMeta: Record<
  NumericOrderField | 'maker_price_anchor',
  { label: string; hint: string; step?: string; min?: string }
> = {
  single_order_usdt: {
    label: '单笔名义金额',
    hint: '每笔 maker 订单的目标 USDT 名义。Exec 会把调仓需求拆成多笔该大小的订单。',
    step: '1',
    min: '1',
  },
  orders_per_batch: {
    label: '每批订单数',
    hint: '同一时刻并行挂出的 maker 数量。数值越大，单次推进越快，但挂单更分散。',
    step: '1',
    min: '1',
  },
  max_batch: {
    label: '最大批次数',
    hint: '单次调仓的目标批次数上限。Exec 按激活目标时的 mark price 放大单笔名义金额，力争在该批数内完成。',
    step: '1',
    min: '1',
  },
  tick_spacing: {
    label: 'Tick 间距',
    hint: '同一批内相邻订单之间的最小 tick 间隔，用于避免多笔挂单挤在同一价位。',
    step: '1',
    min: '0',
  },
  batch_interval_ms: {
    label: '批次间隔 (ms)',
    hint: '两批订单之间的最小等待时间。过小会让撤挂过于频繁，过大则调仓变慢。',
    step: '1',
    min: '0',
  },
  maker_timeout_ms: {
    label: 'Maker 超时 (ms)',
    hint: '单笔 maker 的最长等待时间。超时后会撤单并按规则重报价。',
    step: '1',
    min: '1',
  },
  max_maker_requotes: {
    label: '最大重报价次数',
    hint: '针对同一调仓目标允许的重报价轮次上限，防止长时间反复撤挂。',
    step: '1',
    min: '0',
  },
  target_tolerance_usdt: {
    label: '目标容差 (USDT)',
    hint: '当前持仓与目标仓位差值低于此阈值时，Exec 认为已足够接近，不再继续下单。',
    step: '1',
    min: '0',
  },
  maker_price_anchor: {
    label: 'Maker 价格锚点',
    hint: '决定 maker 报价参考哪一侧盘口。己方一档更保守；对手一档 + 1 tick 更积极。',
  },
}

export const makerPriceAnchorOptions = [
  { value: 'own_best', label: '己方一档', hint: '挂在己方最优价，成交优先级较低，但价格更保守。' },
  {
    value: 'opposite_best_plus_one_tick',
    label: '对手一档 + 1 tick',
    hint: '挂在对手最优价外侧一格，更容易成交，适合需要更快跟仓的场景。',
  },
] as const
