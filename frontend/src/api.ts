import { normalizeTargetMap } from './lib/targetPositions'
import type {
  AccountStudio,
  CatalogOrderStrategy,
  DashboardSnapshot,
  HealthResponse,
  OrderParameters,
  OrderStrategyList,
  OrderStrategyView,
  PositionStrategy,
  SavedPositionStrategy,
  SavedSymbolContractLeverage,
  ExecutionCostSnapshot,
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
}

async function requestJson<T>(path: string, options: RequestOptions = {}): Promise<T> {
  const headers: Record<string, string> = { Accept: 'application/json' }
  if (options.body !== undefined) headers['Content-Type'] = 'application/json'
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

export interface ExecutionCostQuery {
  startMs?: number
  endMs?: number
  windowSec?: number
  sourceIds?: string[]
  strategyName?: string
  signal?: AbortSignal
}

export function getExecutionCost(query: ExecutionCostQuery) {
  const params = new URLSearchParams()
  if (query.startMs !== undefined) params.set('startMs', String(query.startMs))
  if (query.endMs !== undefined) params.set('endMs', String(query.endMs))
  params.set('windowSec', String(query.windowSec ?? 300))
  if (query.sourceIds?.length) params.set('sourceIds', query.sourceIds.join(','))
  if (query.strategyName?.trim()) params.set('strategyName', query.strategyName.trim())
  return requestJson<ExecutionCostSnapshot>(`/catalog/execution-cost?${params}`, {
    signal: query.signal,
  })
}

export function authenticateOrderConfig(signal?: AbortSignal) {
  return requestJson<{ ok: boolean }>('/order-config/auth', {
    method: 'POST',
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
) {
  return requestJson<OrderStrategyView>(
    `/order-config/${encodeURIComponent(sourceId)}/order-parameters`,
    {
      method: 'POST',
      body: {
        strategy_name: strategyName,
        expected_updated_at_us: expectedUpdatedAtUs,
        order_parameters: orderParameters,
      },
    },
  )
}

function decodePositionStrategy(raw: PositionStrategy): PositionStrategy {
  return { ...raw, targets: normalizeTargetMap(raw.targets) }
}

export async function listPositionStrategies(signal?: AbortSignal) {
  const strategies = await requestJson<PositionStrategy[]>('/catalog/position-strategies', {
    signal,
  })
  return strategies.map(decodePositionStrategy)
}

export async function savePositionStrategy(body: PositionStrategy) {
  const saved = await requestJson<SavedPositionStrategy>('/catalog/position-strategies', {
    method: 'POST',
    body: {
      strategy_name: body.strategy_name,
      targets: body.targets,
    },
  })
  return {
    ...decodePositionStrategy(saved),
    publishes: saved.publishes ?? [],
  }
}

export function deletePositionStrategy(name: string) {
  return requestJson<void>(`/catalog/position-strategies/${encodeURIComponent(name)}`, {
    method: 'DELETE',
  })
}

export function listOrderStrategies(signal?: AbortSignal) {
  return requestJson<CatalogOrderStrategy[]>('/catalog/order-strategies', { signal })
}

export function saveOrderStrategy(body: CatalogOrderStrategy) {
  return requestJson<CatalogOrderStrategy>('/catalog/order-strategies', {
    method: 'POST',
    body: {
      strategy_name: body.strategy_name,
      order_parameters: body.order_parameters,
    },
  })
}

export function deleteOrderStrategy(name: string) {
  return requestJson<void>(`/catalog/order-strategies/${encodeURIComponent(name)}`, {
    method: 'DELETE',
  })
}

export function getAccountStudio(sourceId: string, signal?: AbortSignal) {
  return requestJson<AccountStudio>(`/catalog/accounts/${encodeURIComponent(sourceId)}`, {
    signal,
  })
}

export function saveAccountEstimatedFeeRate(sourceId: string, estimatedFeeRate: number) {
  return requestJson<AccountStudio>(
    `/catalog/accounts/${encodeURIComponent(sourceId)}/estimated-fee-rate`,
    {
      method: 'PUT',
      body: { estimated_fee_rate: estimatedFeeRate },
    },
  )
}

export function getAccountContractLeverage(sourceId: string, symbol: string) {
  const params = new URLSearchParams({ symbol })
  return requestJson<SavedSymbolContractLeverage>(
    `/catalog/accounts/${encodeURIComponent(sourceId)}/contract-leverage?${params}`,
  )
}

export function saveAccountContractLeverage(
  sourceId: string,
  symbol: string,
  contractLeverage: number,
) {
  return requestJson<SavedSymbolContractLeverage>(
    `/catalog/accounts/${encodeURIComponent(sourceId)}/contract-leverage`,
    {
      method: 'PUT',
      body: { symbol, contract_leverage: contractLeverage },
    },
  )
}

export function saveAccountBinding(
  sourceId: string,
  bindingName: string,
  positionStrategyName: string,
  orderStrategyName: string,
  shares = 1,
) {
  return requestJson<AccountStudio>(
    `/catalog/accounts/${encodeURIComponent(sourceId)}/bindings`,
    {
      method: 'POST',
      body: {
        binding_name: bindingName,
        position_strategy_name: positionStrategyName,
        order_strategy_name: orderStrategyName,
        shares,
      },
    },
  )
}

export function saveBindingShares(sourceId: string, bindingName: string, shares: number) {
  return requestJson<AccountStudio>(
    `/catalog/accounts/${encodeURIComponent(sourceId)}/bindings/${encodeURIComponent(bindingName)}/shares`,
    {
      method: 'PUT',
      body: { shares },
    },
  )
}

export function deleteAccountBinding(sourceId: string, bindingName: string) {
  return requestJson<void>(
    `/catalog/accounts/${encodeURIComponent(sourceId)}/bindings/${encodeURIComponent(bindingName)}`,
    { method: 'DELETE' },
  )
}

export function publishAccountBinding(sourceId: string, bindingName: string) {
  return requestJson<OrderStrategyView>(
    `/catalog/accounts/${encodeURIComponent(sourceId)}/bindings/${encodeURIComponent(bindingName)}/publish`,
    { method: 'POST' },
  )
}
