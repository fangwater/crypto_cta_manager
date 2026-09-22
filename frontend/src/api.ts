import { normalizeTargetMap } from './lib/targetPositions'
import type {
  AccountStudio,
  CatalogOrderStrategy,
  DashboardSnapshot,
  ExecOrderRateLimits,
  HealthResponse,
  OrderParameters,
  OrderStrategyList,
  OrderStrategyView,
  PositionStrategy,
  SavedPositionStrategy,
  SavedSymbolContractLeverage,
  ExecutionCostSnapshot,
  AcquisitionCostSnapshot,
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
  headers?: Record<string, string>
}

export interface SourceGrant {
  source_id: string
  access_level: 'view' | 'configure'
}

export interface AuthUser {
  user_id: number
  username: string
  role: 'admin' | 'user'
  source_grants: SourceGrant[]
}

export interface AuthStatus {
  authenticated: boolean
  setup_required: boolean
  user: AuthUser | null
}

async function requestJson<T>(path: string, options: RequestOptions = {}): Promise<T> {
  const headers: Record<string, string> = { Accept: 'application/json' }
  Object.assign(headers, options.headers)
  if (options.body !== undefined) headers['Content-Type'] = 'application/json'
  const response = await fetch(API_BASE + path, {
    method: options.method ?? 'GET',
    headers,
    body: options.body === undefined ? undefined : JSON.stringify(options.body),
    cache: 'no-store',
    credentials: 'same-origin',
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

export function getAuthStatus(signal?: AbortSignal) {
  return requestJson<AuthStatus>('/auth/status', { signal })
}

export function register(username: string, password: string) {
  return requestJson<{ user: AuthUser }>('/auth/register', {
    method: 'POST',
    body: { username, password },
  })
}

export function login(username: string, password: string) {
  return requestJson<{ user: AuthUser }>('/auth/login', {
    method: 'POST',
    body: { username, password },
  })
}

export function logout() {
  return requestJson<void>('/auth/logout', { method: 'POST' })
}

export function listAuthUsers(signal?: AbortSignal) {
  return requestJson<AuthUser[]>('/auth/users', { signal })
}

export function createAuthUser(username: string, password: string) {
  return requestJson<AuthUser>('/auth/users', {
    method: 'POST',
    body: { username, password },
  })
}

export function setAuthUserSources(userId: number, grants: SourceGrant[]) {
  return requestJson<AuthUser>(`/auth/users/${userId}/sources`, {
    method: 'PUT',
    body: { grants },
  })
}

export interface AccountGrant {
  user_id: number
  username: string
  access_level: 'view' | 'configure'
}

export function listAccountGrants(sourceId: string, signal?: AbortSignal) {
  return requestJson<AccountGrant[]>(
    `/catalog/accounts/${encodeURIComponent(sourceId)}/grants`,
    { signal },
  )
}

export function setAccountGrants(
  sourceId: string,
  grants: Array<{ user_id: number; access_level: 'view' | 'configure' }>,
) {
  return requestJson<AccountGrant[]>(
    `/catalog/accounts/${encodeURIComponent(sourceId)}/grants`,
    { method: 'PUT', body: { grants } },
  )
}

export function setAuthUserRole(userId: number, role: 'admin' | 'user') {
  return requestJson<AuthUser>(`/auth/users/${userId}/role`, {
    method: 'PUT',
    body: { role },
  })
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
  return getTimelineFromPath('/timeline', query)
}

export function getAccountTimeline(query: TimelineQuery) {
  return getTimelineFromPath('/account-timeline', query)
}

function getTimelineFromPath(path: string, query: TimelineQuery) {
  const params = new URLSearchParams()
  if (query.startMs !== undefined) params.set('startMs', String(query.startMs))
  if (query.endMs !== undefined) params.set('endMs', String(query.endMs))
  if (query.sourceIds?.length) params.set('sourceIds', query.sourceIds.join(','))
  if (query.symbols?.length) params.set('symbols', query.symbols.join(','))
  params.set('maxPoints', String(query.maxPoints ?? 3_000))
  return requestJson<TimelineSnapshot>(`${path}?${params}`, {
    signal: query.signal,
  })
}

export interface ExecutionCostQuery {
  startMs?: number
  endMs?: number
  windowSec?: number
  sourceIds?: string[]
  strategyName?: string
  page?: number
  pageSize?: number
  signal?: AbortSignal
}

export function getExecutionCost(query: ExecutionCostQuery) {
  const params = new URLSearchParams()
  if (query.startMs !== undefined) params.set('startMs', String(query.startMs))
  if (query.endMs !== undefined) params.set('endMs', String(query.endMs))
  params.set('windowSec', String(query.windowSec ?? 300))
  if (query.sourceIds?.length) params.set('sourceIds', query.sourceIds.join(','))
  if (query.strategyName?.trim()) params.set('strategyName', query.strategyName.trim())
  params.set('page', String(query.page ?? 1))
  params.set('pageSize', String(query.pageSize ?? 25))
  return requestJson<ExecutionCostSnapshot>(`/catalog/execution-cost?${params}`, {
    signal: query.signal,
  })
}

export function getAcquisitionCost(query: Omit<ExecutionCostQuery, 'windowSec'>) {
  const params = new URLSearchParams()
  params.set('startMs', String(query.startMs))
  params.set('endMs', String(query.endMs))
  if (query.sourceIds?.length) params.set('sourceIds', query.sourceIds.join(','))
  if (query.strategyName) params.set('strategyName', query.strategyName)
  if (query.page) params.set('page', String(query.page))
  if (query.pageSize) params.set('pageSize', String(query.pageSize))
  return requestJson<AcquisitionCostSnapshot>(`/catalog/acquisition-cost?${params}`, {
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
  experimentalAlgorithmToken?: string,
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
      headers: experimentalAlgorithmToken
        ? { 'X-Experimental-Algorithm-Token': experimentalAlgorithmToken }
        : undefined,
    },
  )
}

function decodePositionStrategy(raw: PositionStrategy): PositionStrategy {
  return {
    ...raw,
    targets: normalizeTargetMap(raw.targets),
    symbol_order_strategy_overrides: raw.symbol_order_strategy_overrides ?? {},
  }
}

export async function listPositionStrategies(signal?: AbortSignal) {
  const strategies = await requestJson<PositionStrategy[]>('/catalog/position-strategies', {
    signal,
  })
  return strategies.map(decodePositionStrategy)
}

export async function savePositionStrategy(
  body: PositionStrategy,
  experimentalAlgorithmToken?: string,
) {
  const saved = await requestJson<SavedPositionStrategy>('/catalog/position-strategies', {
    method: 'POST',
    body: {
      strategy_name: body.strategy_name,
      targets: body.targets,
      symbol_order_strategy_overrides: body.symbol_order_strategy_overrides,
    },
    headers: experimentalAlgorithmToken
      ? { 'X-Experimental-Algorithm-Token': experimentalAlgorithmToken }
      : undefined,
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

export interface PositionManager {
  user_id: number
  username: string
}

export interface PositionGrant {
  user_id: number
  username: string
  access_level: 'view' | 'configure'
}

export interface PositionAccess {
  strategy_name: string
  created_by: string | null
  publish_token_set: boolean
  /** True = every logged-in user can see it. False = private to admins, the creator, granted users, and managers. New strategies default to private. */
  open_visibility: boolean
  managers: PositionManager[]
  grants: PositionGrant[]
}

export function listPositionAccess(signal?: AbortSignal) {
  return requestJson<PositionAccess[]>('/catalog/position-strategies-access', { signal })
}

export function setPositionPublishToken(strategyName: string, publishToken: string) {
  return requestJson<PositionAccess>(
    `/catalog/position-strategies/${encodeURIComponent(strategyName)}/publish-token`,
    { method: 'PUT', body: { publish_token: publishToken } },
  )
}

export function resetPositionPublishToken(strategyName: string) {
  return requestJson<{ publish_token: string; access: PositionAccess }>(
    `/catalog/position-strategies/${encodeURIComponent(strategyName)}/publish-token/reset`,
    { method: 'POST' },
  )
}

export function setPositionManagers(strategyName: string, userIds: number[]) {
  return requestJson<PositionAccess>(
    `/catalog/position-strategies/${encodeURIComponent(strategyName)}/managers`,
    { method: 'PUT', body: { user_ids: userIds } },
  )
}

export function setPositionGrants(
  strategyName: string,
  grants: Array<{ user_id: number; access_level: 'view' | 'configure' }>,
  openVisibility: boolean,
) {
  return requestJson<PositionAccess>(
    `/catalog/position-strategies/${encodeURIComponent(strategyName)}/grants`,
    { method: 'PUT', body: { grants, open_visibility: openVisibility } },
  )
}

export interface PublishToken {
  token_id: number
  note: string
  created_at: string
}

export function listPublishTokens(signal?: AbortSignal) {
  return requestJson<PublishToken[]>('/catalog/publish-tokens', { signal })
}

export function addPublishToken(note: string, publishToken?: string) {
  return requestJson<PublishToken & { publish_token: string }>(
    '/catalog/publish-tokens',
    {
      method: 'POST',
      body: publishToken ? { note, publish_token: publishToken } : { note },
    },
  )
}

export function deletePublishToken(tokenId: number) {
  return requestJson<void>(`/catalog/publish-tokens/${tokenId}`, { method: 'DELETE' })
}

export async function listOrderStrategies(signal?: AbortSignal) {
  const strategies = await requestJson<CatalogOrderStrategy[]>('/catalog/order-strategies', { signal })
  return strategies
}

export function saveOrderStrategy(body: CatalogOrderStrategy, experimentalAlgorithmToken?: string) {
  return requestJson<CatalogOrderStrategy>('/catalog/order-strategies', {
    method: 'POST',
    body: {
      strategy_name: body.strategy_name,
      order_parameters: body.order_parameters,
    },
    headers: experimentalAlgorithmToken
      ? { 'X-Experimental-Algorithm-Token': experimentalAlgorithmToken }
      : undefined,
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

export function saveAccountFeeRates(
  sourceId: string,
  makerFeeRate: number,
  takerFeeRate: number,
  theoreticalTwapFeeRate: number,
) {
  return requestJson<AccountStudio>(
    `/catalog/accounts/${encodeURIComponent(sourceId)}/fee-rates`,
    {
      method: 'PUT',
      body: {
        maker_fee_rate: makerFeeRate,
        taker_fee_rate: takerFeeRate,
        theoretical_twap_fee_rate: theoreticalTwapFeeRate,
      },
    },
  )
}

export function getAccountContractLeverage(sourceId: string, symbol: string) {
  const params = new URLSearchParams({ symbol })
  return requestJson<SavedSymbolContractLeverage>(
    `/catalog/accounts/${encodeURIComponent(sourceId)}/contract-leverage?${params}`,
  )
}

export function getAccountExecOrderRateLimits(sourceId: string, signal?: AbortSignal) {
  return requestJson<ExecOrderRateLimits>(
    `/catalog/accounts/${encodeURIComponent(sourceId)}/exec-order-rate-limits`,
    { signal },
  )
}

export function saveAccountExecOrderRateLimits(
  sourceId: string,
  limitPerMin: number,
  limit10s: number,
) {
  return requestJson<ExecOrderRateLimits>(
    `/catalog/accounts/${encodeURIComponent(sourceId)}/exec-order-rate-limits`,
    {
      method: 'PUT',
      body: {
        exec_order_rate_limit_per_min: limitPerMin,
        exec_order_rate_limit_10s: limit10s,
      },
    },
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
  experimentalAlgorithmToken?: string,
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
      headers: experimentalAlgorithmToken
        ? { 'X-Experimental-Algorithm-Token': experimentalAlgorithmToken }
        : undefined,
    },
  )
}

export function saveBindingShares(
  sourceId: string,
  bindingName: string,
  shares: number,
  experimentalAlgorithmToken?: string,
) {
  return requestJson<AccountStudio>(
    `/catalog/accounts/${encodeURIComponent(sourceId)}/bindings/${encodeURIComponent(bindingName)}/shares`,
    {
      method: 'PUT',
      body: { shares },
      headers: experimentalAlgorithmToken
        ? { 'X-Experimental-Algorithm-Token': experimentalAlgorithmToken }
        : undefined,
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
