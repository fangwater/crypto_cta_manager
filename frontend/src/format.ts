const moneyFormatter = new Intl.NumberFormat('zh-CN', {
  minimumFractionDigits: 2,
  maximumFractionDigits: 2,
})

const compactMoneyFormatter = new Intl.NumberFormat('zh-CN', {
  notation: 'compact',
  minimumFractionDigits: 0,
  maximumFractionDigits: 2,
})

const quantityFormatter = new Intl.NumberFormat('zh-CN', {
  minimumFractionDigits: 0,
  maximumFractionDigits: 8,
})

const integerFormatter = new Intl.NumberFormat('zh-CN', {
  maximumFractionDigits: 0,
})

const timeFormatter = new Intl.DateTimeFormat('zh-CN', {
  month: '2-digit',
  day: '2-digit',
  hour: '2-digit',
  minute: '2-digit',
  second: '2-digit',
  hour12: false,
})

export function money(value: number) {
  return moneyFormatter.format(value)
}

export function compactMoney(value: number) {
  return compactMoneyFormatter.format(value)
}

export function quantity(value: number) {
  return quantityFormatter.format(value)
}

export function integer(value: number) {
  return integerFormatter.format(value)
}

export function timestampUs(value: number | null) {
  return value === null ? '--' : timeFormatter.format(value / 1_000)
}

export function feeBps(rate: number) {
  return `${(rate * 10_000).toFixed(2)} bps`
}

export function signedClass(value: number) {
  if (value > 0) return 'number-positive'
  if (value < 0) return 'number-negative'
  return ''
}

export function strategyLabel(strategy: string) {
  if (strategy === '__initial_position__') return '初始仓位（未归属）'
  if (strategy === '__unattributed__') return '未归属成交'
  if (strategy.toLowerCase() === 'system_position_close') return '系统平仓'
  return strategy
}
