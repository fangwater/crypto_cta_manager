export const ALLOWED_TARGET_SIGNALS = [-2, -1, 0, 1, 2] as const

export type TargetSignal = (typeof ALLOWED_TARGET_SIGNALS)[number]

export interface TargetPosition {
  qty: number
  signal: TargetSignal
}

export type TargetMap = Record<string, TargetPosition>

export function isTargetSignal(value: number): value is TargetSignal {
  return (ALLOWED_TARGET_SIGNALS as readonly number[]).includes(value)
}

export function normalizeTargetPosition(raw: unknown): TargetPosition | null {
  if (typeof raw === 'number') {
    if (!Number.isFinite(raw)) return null
    return { qty: raw, signal: 0 }
  }
  if (!raw || typeof raw !== 'object') return null
  const qty = Number((raw as { qty?: unknown }).qty)
  const signalRaw = (raw as { signal?: unknown }).signal
  const signal = signalRaw == null || signalRaw === '' ? 0 : Number(signalRaw)
  if (!Number.isFinite(qty) || !Number.isInteger(signal) || !isTargetSignal(signal)) {
    return null
  }
  return { qty, signal }
}

export function normalizeTargetMap(raw: unknown): TargetMap {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return {}
  const next: TargetMap = {}
  for (const [symbol, value] of Object.entries(raw as Record<string, unknown>)) {
    const target = normalizeTargetPosition(value)
    if (target) next[symbol] = target
  }
  return next
}

export function targetQty(target: TargetPosition | number | undefined) {
  if (target == null) return 0
  return typeof target === 'number' ? target : target.qty
}

export function signalLabel(signal: number) {
  if (signal === 1 || signal === -1) return '本轮 taker'
  if (signal === 0) return '默认'
  return `signal ${signal}`
}
