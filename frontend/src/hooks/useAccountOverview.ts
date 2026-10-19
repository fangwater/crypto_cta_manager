import { useCallback, useEffect, useRef, useState } from 'react'
import { getAccountOverview } from '../api'
import type { AccountOverview } from '../lib/accountOverview'

export function useAccountOverview(date: string) {
  const [data, setData] = useState<AccountOverview | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [refreshing, setRefreshing] = useState(false)
  const [autoRefresh, setAutoRefresh] = useState(true)
  const [countdown, setCountdown] = useState(15)
  const request = useRef<AbortController | null>(null)
  const refresh = useCallback(async () => {
    if (request.current) return
    const controller = new AbortController()
    request.current = controller
    setRefreshing(true)
    try {
      const result = await getAccountOverview(date, controller.signal)
      if (!controller.signal.aborted) { setData(result); setError(null) }
    } catch (error) {
      if (!controller.signal.aborted) setError(error instanceof Error ? error.message : String(error))
    } finally {
      if (request.current === controller) request.current = null
      if (!controller.signal.aborted) { setRefreshing(false); setCountdown(15) }
    }
  }, [date])
  useEffect(() => {
    setData(null)
    setError(null)
    void refresh()
    return () => { request.current?.abort(); request.current = null }
  }, [refresh])
  useEffect(() => {
    if (!autoRefresh || refreshing) return
    if (countdown === 0) { void refresh(); return }
    const timer = window.setTimeout(() => setCountdown(value => value - 1), 1000)
    return () => window.clearTimeout(timer)
  }, [autoRefresh, countdown, refreshing, refresh])
  return { data, error, refreshing, autoRefresh, setAutoRefresh, countdown, refresh }
}
