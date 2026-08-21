import type { OrderParameters } from '../types'

export function maxEstimatedExecutionMs(parameters: OrderParameters): number {
  const maxBatch = Math.max(1, Math.trunc(parameters.max_batch || 0))
  const batchIntervalMs = Math.max(0, parameters.batch_interval_ms || 0)
  const makerAttempts = Math.max(1, Math.trunc(parameters.max_maker_requotes || 0) + 1)
  const makerTimeoutMs = Math.max(0, parameters.maker_timeout_ms || 0)

  return (maxBatch - 1) * batchIntervalMs + makerAttempts * makerTimeoutMs
}

export function formatDuration(milliseconds: number): string {
  const totalSeconds = Math.max(0, milliseconds) / 1_000
  if (totalSeconds < 60) return `${Number(totalSeconds.toFixed(1))} 秒`

  const minutes = Math.floor(totalSeconds / 60)
  const seconds = Math.round(totalSeconds % 60)
  return seconds === 0 ? `${minutes} 分钟` : `${minutes} 分 ${seconds} 秒`
}
