import type { AcquisitionCostRow } from '../types'

// Compare related prices at one precision, retaining two digits of even the
// smallest observed difference. This is display precision, not an exchange tick.
export function priceFormatter(values: (number | null)[]) {
  const finite = values.filter((v): v is number => v != null && Number.isFinite(v))
  const nonzero = finite.map(Math.abs).filter((v) => v > 0)
  const scale = nonzero.length ? Math.min(...nonzero) : 1
  let digits = Math.max(2, 9 - Math.floor(Math.log10(scale)))
  const sorted = [...new Set(finite)].sort((a, b) => a - b)
  for (let i = 1; i < sorted.length; i++) {
    const gap = sorted[i] - sorted[i - 1]
    // Ignore floating-point roundoff rather than displaying fictitious precision.
    if (gap > Math.max(Math.abs(sorted[i]), Math.abs(sorted[i - 1])) * 1e-13) {
      digits = Math.max(digits, 1 - Math.floor(Math.log10(gap)))
    }
  }
  const formatter = new Intl.NumberFormat('en-US', {
    minimumFractionDigits: Math.min(16, digits),
    maximumFractionDigits: Math.min(16, digits),
  })
  return (value: number | null | undefined) => {
    if (value == null || !Number.isFinite(value)) return '--'
    if (value !== 0 && Math.abs(value) < 1e-12) return value.toExponential(9)
    return formatter.format(Object.is(value, -0) ? 0 : value)
  }
}

export function rowPriceFormatter(row: AcquisitionCostRow) {
  return priceFormatter([row.virtual_vwap, row.actual_vwap, ...row.sample_prices])
}

export function deltaKey(row: AcquisitionCostRow) {
  return JSON.stringify([row.source_id, row.binding_name, row.strategy_name, row.symbol, row.venue, row.received_at_us])
}

export function analyzePricePath(row: AcquisitionCostRow) {
  const prices = row.sample_prices
  const traded = row.zero_volume_fallback_sample_count === 0
  const complete = traded && prices.length >= 3 && prices.every((price) => price != null && price > 0)
  let jumpBps: number | null = null
  let jumpMinute: number | null = null
  if (traded) {
    for (let i = 1; i < prices.length; i++) {
      const before = prices[i - 1]
      const after = prices[i]
      if (before == null || before <= 0 || after == null || after <= 0) continue
      const jump = (after / before - 1) * 10_000
      if (jumpBps == null || Math.abs(jump) > Math.abs(jumpBps)) {
        jumpBps = jump
        jumpMinute = i + 1
      }
    }
  }
  let reversal: { outwardBps: number; returnBps: number; minute: number } | null = null
  if (complete) {
    const first = prices[0]!
    const last = prices[prices.length - 1]!
    for (let i = 1; i < prices.length - 1; i++) {
      const pivot = prices[i]!
      const outwardBps = (pivot - first) / first * 10_000
      const returnBps = (last - pivot) / pivot * 10_000
      if (outwardBps * returnBps >= 0) continue
      const strength = Math.min(Math.abs(outwardBps), Math.abs(returnBps))
      if (!reversal || strength > Math.min(Math.abs(reversal.outwardBps), Math.abs(reversal.returnBps))) {
        reversal = { outwardBps, returnBps, minute: i + 1 }
      }
    }
  }
  const first = prices.find((price) => price != null && price > 0)
  const direction = Math.sign(row.delta_qty)
  return {
    complete, jumpBps, jumpMinute, reversal,
    versusFirstBps: first != null && row.actual_vwap != null
      ? direction * (row.actual_vwap - first) / row.virtual_vwap * 10_000 : null,
    pathBps: first != null ? direction * (first - row.virtual_vwap) / row.virtual_vwap * 10_000 : null,
  }
}
