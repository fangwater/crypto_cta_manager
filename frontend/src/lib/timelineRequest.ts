import { decodeTimeline } from './timelineProto.ts'
import type { TimelinePayload } from './timelineProto'
import { parseApiError } from './apiError.ts'
import type { ApiErrorFactory } from './apiError'

export type { PricingState } from './apiError'
export type TimelineErrorFactory = ApiErrorFactory

export async function fetchTimeline(
  url: string, signal: AbortSignal | undefined, error: TimelineErrorFactory,
  observeDate: (value: string | null) => void = () => {},
): Promise<TimelinePayload> {
  const response = await fetch(url, {
    headers: { Accept: 'application/x-protobuf' },
    cache: 'no-store', credentials: 'same-origin', signal,
  })
  observeDate(response.headers.get('Date'))
  if (!response.ok) {
    const payload = await response.json().catch(() => null) as
      { error?: string; code?: string; loading?: boolean; available_from_us?: number } | null
    throw parseApiError(response.status, payload, error)
  }
  if (response.headers.get('Content-Type')?.split(';')[0] !== 'application/x-protobuf') throw new Error('净值响应格式不匹配，请刷新页面')
  return decodeTimeline(new Uint8Array(await response.arrayBuffer()))
}
