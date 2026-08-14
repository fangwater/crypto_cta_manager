import type {
  DashboardSnapshot,
  HealthResponse,
  TimelineSnapshot,
} from './types'

const API_BASE = import.meta.env.VITE_CTA_API_BASE ?? '/manager/api'

async function getJson<T>(path: string, signal?: AbortSignal): Promise<T> {
  const response = await fetch(API_BASE + path, {
    headers: { Accept: 'application/json' },
    cache: 'no-store',
    signal,
  })
  if (!response.ok) {
    const payload = (await response.json().catch(() => null)) as
      | { error?: string }
      | null
    throw new Error(payload?.error ?? `HTTP ${response.status}`)
  }
  return response.json() as Promise<T>
}

export function getDashboard(signal?: AbortSignal) {
  return getJson<DashboardSnapshot>('/dashboard', signal)
}

export function getHealth(signal?: AbortSignal) {
  return getJson<HealthResponse>('/health', signal)
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
  return getJson<TimelineSnapshot>(`/timeline?${params}`, query.signal)
}
