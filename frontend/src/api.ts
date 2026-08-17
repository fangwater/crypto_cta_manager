import type {
  AccountStudio,
  CatalogOrderStrategy,
  DashboardSnapshot,
  HealthResponse,
  OrderParameters,
  OrderStrategyList,
  OrderStrategyView,
  PositionStrategy,
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
  method?: 'GET' | 'POST' | 'PUT' | 'DELETE'
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
  if (response.status === 204) return undefined as T
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

export function listPositionStrategies(signal?: AbortSignal) {
  return requestJson<PositionStrategy[]>('/catalog/position-strategies', { signal })
}

export function savePositionStrategy(body: PositionStrategy, token: string) {
  return requestJson<PositionStrategy>('/catalog/position-strategies', {
    method: 'POST',
    token,
    body: {
      strategy_name: body.strategy_name,
      equity_usdt: body.equity_usdt,
      targets: body.targets,
    },
  })
}

export function deletePositionStrategy(name: string, token: string) {
  return requestJson<void>(`/catalog/position-strategies/${encodeURIComponent(name)}`, {
    method: 'DELETE',
    token,
  })
}

export function listOrderStrategies(signal?: AbortSignal) {
  return requestJson<CatalogOrderStrategy[]>('/catalog/order-strategies', { signal })
}

export function saveOrderStrategy(body: CatalogOrderStrategy, token: string) {
  return requestJson<CatalogOrderStrategy>('/catalog/order-strategies', {
    method: 'POST',
    token,
    body: {
      strategy_name: body.strategy_name,
      order_parameters: body.order_parameters,
    },
  })
}

export function deleteOrderStrategy(name: string, token: string) {
  return requestJson<void>(`/catalog/order-strategies/${encodeURIComponent(name)}`, {
    method: 'DELETE',
    token,
  })
}

export function getAccountStudio(sourceId: string, signal?: AbortSignal) {
  return requestJson<AccountStudio>(`/catalog/accounts/${encodeURIComponent(sourceId)}`, {
    signal,
  })
}

export function saveAccountStudio(sourceId: string, leverage: number, token: string) {
  return requestJson<AccountStudio>(`/catalog/accounts/${encodeURIComponent(sourceId)}`, {
    method: 'PUT',
    token,
    body: { leverage },
  })
}

export function saveAccountBinding(
  sourceId: string,
  bindingName: string,
  positionStrategyName: string,
  orderStrategyName: string,
  token: string,
) {
  return requestJson<AccountStudio>(
    `/catalog/accounts/${encodeURIComponent(sourceId)}/bindings`,
    {
      method: 'POST',
      token,
      body: {
        binding_name: bindingName,
        position_strategy_name: positionStrategyName,
        order_strategy_name: orderStrategyName,
      },
    },
  )
}

export function deleteAccountBinding(sourceId: string, bindingName: string, token: string) {
  return requestJson<void>(
    `/catalog/accounts/${encodeURIComponent(sourceId)}/bindings/${encodeURIComponent(bindingName)}`,
    { method: 'DELETE', token },
  )
}

export function publishAccountBinding(sourceId: string, bindingName: string, token: string) {
  return requestJson<OrderStrategyView>(
    `/catalog/accounts/${encodeURIComponent(sourceId)}/bindings/${encodeURIComponent(bindingName)}/publish`,
    { method: 'POST', token },
  )
}
