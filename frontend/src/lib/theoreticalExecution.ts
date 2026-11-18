import type { TheoreticalExecutionModel } from '../types'

export function isNativeExecutionModel(model?: TheoreticalExecutionModel | null) {
  return model?.startsWith('native_') === true
}

export function nativeOffsetLabel(offset?: string) {
  switch (offset) {
    case 'open': return '开仓'
    case 'close': return '平仓'
    case 'close_today': return '平今'
    case 'close_yesterday': return '平昨'
    default: return null
  }
}

export function theoreticalExecutionLabel(model?: TheoreticalExecutionModel | null) {
  if (model === 'native_twap') return '合约 TWAP'
  if (model === 'native_vwap') return '合约 VWAP'
  if (model === 'native_reference') return '合约参考价'
  if (model === 'vwap3m') return '理论3min_vwap'
  if (model === 'twap3m') return '理论3min_twap'
  if (model === 'instant_taker') return '理论瞬时taker'
  if (model === 'twap5m') return '理论 TWAP（5 分钟）'
  return '理论成交价'
}

export function theoreticalExecutionDescription(model?: TheoreticalExecutionModel | null) {
  if (isNativeExecutionModel(model)) return '按原生合约和交易时段比较实际成交价与参考价。实际手续费取成交回报，理论手续费按对应开平的每手费额或成交额费率核算。金额为人民币，数量为手。'
  if (model === 'instant_taker') return '以信号接收时间所在整分钟的 K 线 open 估计即时成交价，在接收时一次性计入理论数量，费用使用账户 taker 费率。未计入买卖价差与市场冲击；沿用已收盘分钟缓存，仅支持最近 30 天。'
  if (model === 'vwap3m') return 'RapidX 使用目标发布后首个完整分钟开始的连续三个分钟，总成交额 ÷ 总成交币数量得到理论 VWAP；分钟数量权重为该分钟成交量占比。按实际成交数量比较价格与费用，仅支持最近 30 天。'
  if (model === 'twap3m') return '理论3min_twap 将连续三个完整分钟的分钟均价（各分钟成交额 ÷ 成交量）等权平均。每分钟分配三分之一理论数量。与理论3min_vwap 使用相同窗口，仅支持最近 30 天。'
  if (model === 'twap5m') return '理论 TWAP 使用目标发布后五个完整分钟的 VWAP（成交额 ÷ 成交量），跳过无成交分钟，在其余分钟均分数量；五分钟全部无成交时使用收盘价。按实际成交数量比较价格与费用，仅支持最近 30 天。'
  return '按合约的交易时段和参考窗口比较实际成交价与理论价。数量以手计，成交额和费用按合约乘数折算为人民币。'
}
