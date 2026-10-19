import { useCallback, useEffect, useRef, useState } from 'react'
import { getDashboard, getHealth } from '../api'
import type { DashboardSnapshot, HealthResponse } from '../types'

export function useWorkspaceData() {
  const [dashboard, setDashboard] = useState<DashboardSnapshot | null>(null)
  const [health, setHealth] = useState<HealthResponse | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [refreshing, setRefreshing] = useState(false)
  const [autoRefresh, setAutoRefresh] = useState(true)
  const [countdown, setCountdown] = useState(15)
  const [receivedAt, setReceivedAt] = useState<number | null>(null)
  const request = useRef<AbortController | null>(null)

  const refresh = useCallback(async () => {
    if (request.current) return
    const controller = new AbortController()
    request.current = controller
    setRefreshing(true)
    try {
      const [nextDashboard, nextHealth] = await Promise.allSettled([
        getDashboard(controller.signal), getHealth(controller.signal),
      ])
      if (controller.signal.aborted) return
      if (nextDashboard.status === 'fulfilled') {
        setDashboard(nextDashboard.value)
        setReceivedAt(Date.now())
      }
      setHealth(nextHealth.status === 'fulfilled' ? nextHealth.value : null)
      const failed = [nextDashboard, nextHealth].find(result => result.status === 'rejected')
      setError(failed?.status === 'rejected' ? String(failed.reason instanceof Error ? failed.reason.message : failed.reason) : null)
    } finally {
      if (request.current === controller) request.current = null
      if (!controller.signal.aborted) {
        setRefreshing(false)
        setCountdown(15)
      }
    }
  }, [])

  useEffect(() => {
    void refresh()
    return () => {
      request.current?.abort()
      request.current = null
    }
  }, [refresh])

  useEffect(() => {
    if (!autoRefresh || refreshing) return
    if (countdown === 0) {
      void refresh()
      return
    }
    const timer = window.setTimeout(() => setCountdown(value => value - 1), 1000)
    return () => window.clearTimeout(timer)
  }, [autoRefresh, countdown, refreshing, refresh])

  return { dashboard, health, error, refreshing, autoRefresh, setAutoRefresh, countdown, receivedAt, refresh }
}
