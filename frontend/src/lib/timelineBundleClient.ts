// The production bundle can replace its NAV request function without rolling out
// unrelated page changes. Both builds use the same decoder and Protobuf contract.
import { fetchTimeline } from './timelineRequest.ts'
import type { PricingState } from './timelineRequest'
import type { TimelineSnapshot } from '../types'

interface Query {
  startMs?: number
  endMs?: number
  sourceIds?: string[]
  symbols?: string[]
  maxPoints?: number
  includeTheoretical?: boolean
  view?: 'portfolio' | 'symbols' | 'strategies'
  signal?: AbortSignal
}
type ApiErrorClass = new (message: string, status: number, pricing?: PricingState) => Error
const factual = new Map<string, TimelineSnapshot>()

export async function requestNavTimeline(path: string, query: Query, ApiError: ApiErrorClass): Promise<TimelineSnapshot> {
  const params = new URLSearchParams()
  if (query.startMs !== undefined) params.set('startMs', String(query.startMs))
  if (query.endMs !== undefined) params.set('endMs', String(query.endMs))
  if (query.sourceIds?.length) params.set('sourceIds', query.sourceIds.join(','))
  if (query.symbols?.length) params.set('symbols', query.symbols.join(','))
  params.set('maxPoints', String(query.maxPoints ?? 3000))
  params.set('view', query.view ?? 'portfolio')
  const key = path + '?' + params
  const loadFactual = async () => {
    params.set('includeTheoretical', 'false')
    const result = await fetchTimeline('/manager/api' + path + '?' + params, query.signal,
      (message, status, pricing) => new ApiError(message, status, pricing))
    if (!result.report) throw new Error('净值响应缺少报表')
    factual.delete(key)
    factual.set(key, result as TimelineSnapshot)
    while (factual.size > 3) factual.delete(factual.keys().next().value!)
    return result as TimelineSnapshot
  }
  if (!query.includeTheoretical) return loadFactual()
  let base = factual.get(key)
  // Funding retries refresh only the small factual projection.
  if (!base || base.estimated_funding?.loading) base = await loadFactual()
  params.set('view', 'theoretical')
  params.set('includeTheoretical', 'true')
  try {
    const overlay = await fetchTimeline('/manager/api' + path + '?' + params, query.signal,
      (message, status, pricing) => new ApiError(message, status, pricing))
    return { ...base, theoretical: overlay.theoretical, execution_slippage: overlay.execution_slippage,
      generation_duration_ms: base.generation_duration_ms + overlay.generation_duration_ms }
  } catch (reason) {
    const status = (reason as { status?: number })?.status
    if (query.signal?.aborted || status === 401 || status === 403) throw reason
    const loading = (reason as { navPricing?: PricingState })?.navPricing?.loading === true
    return { ...base, theoretical: base.theoretical.map(reference => ({
      ...reference, loading, unavailable_reason: '理论分析暂不可用，请稍后重试',
    })) }
  }
}
