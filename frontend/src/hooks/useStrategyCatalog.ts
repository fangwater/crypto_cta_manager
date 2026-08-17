import { useCallback, useEffect, useState } from 'react'
import { listOrderStrategies, listPositionStrategies } from '../api'
import type { CatalogOrderStrategy, PositionStrategy } from '../types'

export function useStrategyCatalog() {
  const [positions, setPositions] = useState<PositionStrategy[]>([])
  const [orders, setOrders] = useState<CatalogOrderStrategy[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const reloadCatalog = useCallback(async (signal?: AbortSignal) => {
    const [nextPositions, nextOrders] = await Promise.all([
      listPositionStrategies(signal),
      listOrderStrategies(signal),
    ])
    setPositions(nextPositions)
    setOrders(nextOrders)
    return { positions: nextPositions, orders: nextOrders }
  }, [])

  useEffect(() => {
    const controller = new AbortController()
    reloadCatalog(controller.signal)
      .then(() => setError(null))
      .catch((reason: unknown) => {
        if (reason instanceof DOMException && reason.name === 'AbortError') return
        setError(reason instanceof Error ? reason.message : String(reason))
      })
      .finally(() => setLoading(false))
    return () => controller.abort()
  }, [reloadCatalog])

  return { positions, orders, loading, error, setError, reloadCatalog }
}
