// Exec identities are stable; account labels and gateway slots can be reused.
const PREP_GROUP_BY_SOURCE: Readonly<Record<string, string>> = {
  binance_exec_rx01: 'prep01',
  binance_exec_rx03: 'prep02',
}

export function prepSignalConfigPath(sourceId: string, strategyName?: string) {
  const group = PREP_GROUP_BY_SOURCE[sourceId]
  if (!group) return undefined

  let interval = '1min'
  if (strategyName !== undefined) {
    const match = /^cta-(prep01|prep02)-bn-(1min|5min|15min)-pos$/.exec(strategyName)
    if (!match || match[1] !== group) return undefined
    interval = match[2]
  }
  const combination = `${group === 'prep01' ? '' : `${group}_`}binance-futures_${interval}`
  return `/cta/cta-prep-binance-futures/config/?worker=cta-prep-binance-futures#${combination}`
}
