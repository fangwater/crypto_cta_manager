import type { TheoreticalExecutionModel } from '../types'

export function theoreticalExecutionLabel(model?: TheoreticalExecutionModel | null) {
  if (model === 'vwap3m') return '理论3min_vwap'
  if (model === 'twap3m') return '理论3min_twap'
  if (model === 'twap5m') return '理论 TWAP（5 分钟）'
  return '理论成交价'
}

export function theoreticalExecutionDescription(model?: TheoreticalExecutionModel | null) {
  if (model === 'vwap3m') return 'RapidX 使用目标发布后首个完整分钟开始的连续三个分钟，总成交额 ÷ 总成交币数量得到理论 VWAP；分钟数量权重为该分钟成交量占比。按实际成交数量比较价格与费用，仅支持最近 30 天。'
  if (model === 'twap3m') return '理论3min_twap 将连续三个完整分钟的分钟均价（各分钟成交额 ÷ 成交量）等权平均。每分钟分配三分之一理论数量。与理论3min_vwap 使用相同窗口，仅支持最近 30 天。'
  if (model === 'twap5m') return '理论 TWAP 使用目标发布后五个完整分钟的 VWAP（成交额 ÷ 成交量），跳过无成交分钟，在其余分钟均分数量；五分钟全部无成交时使用收盘价。按实际成交数量比较价格与费用，仅支持最近 30 天。'
  return 'RapidX 同时提供理论3min_vwap 和理论3min_twap；其他账户使用五分钟理论 TWAP。按实际成交数量比较价格与费用，仅支持最近 30 天。'
}
