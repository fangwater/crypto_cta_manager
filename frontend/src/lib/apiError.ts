export type PricingState = { loading: boolean; availableFromUs: number }
export type ApiErrorFactory = (message: string, status: number, pricing?: PricingState, historyLoading?: boolean) => Error

export class ApiError extends Error {
  readonly status: number
  readonly navPricing?: PricingState
  readonly historyLoading: boolean

  constructor(
    message: string,
    status: number,
    navPricing?: PricingState,
    historyLoading = false,
  ) {
    super(message)
    this.status = status
    this.navPricing = navPricing
    this.historyLoading = historyLoading
  }
}

export function parseApiError(
  status: number,
  payload: { error?: string; code?: string; loading?: boolean; available_from_us?: number } | null,
  create: ApiErrorFactory = (message, status, pricing, historyLoading) => new ApiError(message, status, pricing, historyLoading),
): Error {
  const pricing = payload?.code === 'nav_pricing_unavailable' && typeof payload.available_from_us === 'number'
    ? { loading: payload.loading === true, availableFromUs: payload.available_from_us } : undefined
  return create(payload?.error ?? `HTTP ${status}`, status, pricing,
    status === 503 && payload?.loading === true && payload.code !== 'nav_pricing_unavailable')
}
