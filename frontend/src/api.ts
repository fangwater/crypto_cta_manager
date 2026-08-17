import type {
  DashboardSnapshot,
  HealthResponse,
  OrderParameters,
  OrderStrategyList,
  OrderStrategyView,
  TimelineSnapshot,
} from './types'

const API_BASE = import.meta.env.VITE_CTA_API_BASE ?? '/manager/api'

export class ApiError extends Error {
  constructor(
    message: string,
    public readonly status: number,
  ) {
    super(message)
  }
}

interface RequestOptions {
  signal?: AbortSignal
  method?: 'GET' | 'POST'
  body?: unknown
  token?: string
}

async function requestJson<T>(path: string, options: RequestOptions = {}): Promise<T> {
  const headers: Record<string, string> = { Accept: 'application/json' }
  if (options.body !== undefined) headers['Content-Type'] = 'application/json'
  if (options.token) headers.Authorization = `Bearer ${options.token}`
  const response = await fetch(API_BASE + path, {
    method: options.method ?? 'GET',
    headers,
    body: options.body === undefined ? undefined : JSON.stringify(options.body),
    cache: 'no-store',
    signal: options.signal,
  })
  if (!response.ok) {
    const payload = (await response.json().catch(() => null)) as
      | { error?: string }
      | null
    throw new ApiError(payload?.error ?? `HTTP ${response.status}`, response.status)
  }
  return response.json() as Promise<T>
}

export function getDashboard(signal?: AbortSignal) {
  return requestJson<DashboardSnapshot>('/dashboard', { signal })
}

export function getHealth(signal?: AbortSignal) {
  return requestJson<HealthResponse>('/health', { signal })
}

export interface TimelineQuery {
  startMs?: number
  endMs?: number
  sourceIds?: string[]
  symbols?: string[]
  maxPoints?: number
  signal?: AbortSignal
}

export function getTimeline(query: TimelineQuery) {
  const params = new URLSearchParams()
  if (query.startMs !== undefined) params.set('startMs', String(query.startMs))
  if (query.endMs !== undefined) params.set('endMs', String(query.endMs))
  if (query.sourceIds?.length) params.set('sourceIds', query.sourceIds.join(','))
  if (query.symbols?.length) params.set('symbols', query.symbols.join(','))
  params.set('maxPoints', String(query.maxPoints ?? 3_000))
  return requestJson<TimelineSnapshot>(`/timeline?${params}`, {
    signal: query.signal,
  })
}

export function authenticateOrderConfig(token: string, signal?: AbortSignal) {
  return requestJson<{ ok: boolean }>('/order-config/auth', {
    method: 'POST',
    token,
    signal,
  })
}

export function getOrderConfigStrategies(sourceId: string, signal?: AbortSignal) {
  return requestJson<OrderStrategyList>(
    `/order-config/${encodeURIComponent(sourceId)}/strategies`,
    { signal },
  )
}

export function getOrderConfigStrategy(
  sourceId: string,
  strategyName: string,
  signal?: AbortSignal,
) {
  const query = new URLSearchParams({ name: strategyName })
  return requestJson<OrderStrategyView>(
    `/order-config/${encodeURIComponent(sourceId)}/strategy?${query}`,
    { signal },
  )
}

export function saveOrderParameters(
  sourceId: string,
  strategyName: string,
  expectedUpdatedAtUs: number,
  orderParameters: OrderParameters,
  token: string,
) {
  return requestJson<OrderStrategyView>(
    `/order-config/${encodeURIComponent(sourceId)}/order-parameters`,
    {
      method: 'POST',
      token,
      body: {
        strategy_name: strategyName,
        expected_updated_at_us: expectedUpdatedAtUs,
        order_parameters: orderParameters,
      },
    },
  )
}
