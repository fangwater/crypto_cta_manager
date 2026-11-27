import type { AcquisitionCostSnapshot } from '../types'

// Crypto reports use *_usdt, while domestic reports already send *_quote.
export function normalizeAcquisitionCost(snapshot: unknown): AcquisitionCostSnapshot {
  function normalize(value: unknown): unknown {
    if (Array.isArray(value)) return value.map(normalize)
    if (value === null || typeof value !== 'object') return value

    const entries: [string, unknown][] = []
    for (const [key, field] of Object.entries(value)) {
      const quoteKey = key.replace(/_usdt$/, '_quote')
      if (quoteKey !== key && Object.hasOwn(value, quoteKey)) continue
      entries.push([quoteKey, normalize(field)])
    }
    return Object.fromEntries(entries)
  }

  return normalize(snapshot) as AcquisitionCostSnapshot
}
