import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { DashboardAccount } from '../types'
import { applyExecMessage, emptyExecFeed, type ExecFeed, type ExecSource } from '../lib/execMonitor'
import { serverClock } from '../lib/serverClock'

/** Read-only native streams, joined in memory under the single public account. */
export function useExecMonitor(executions: DashboardAccount[]) {
  const sourceKey = JSON.stringify(executions.map(a => ({ sourceId: a.source_id, venue: a.venue, gatewayPrefix: a.gateway_prefix })))
  const sources = useMemo(() => JSON.parse(sourceKey) as ExecSource[], [sourceKey])
  const [state, setState] = useState<Record<string, ExecFeed>>({})
  const [now, setNow] = useState(() => serverClock.now())
  const refreshRef = useRef<() => void>(() => {})
  const refresh = useCallback(() => refreshRef.current(), [])
  useEffect(() => {
    let active = true
    const controller = new AbortController()
    const cleanups: Array<() => void> = [], refreshers: Array<() => void> = []
    setState(Object.fromEntries(sources.map(s => [s.sourceId, emptyExecFeed(s)])))
    const update = (source: ExecSource, transform: (feed: ExecFeed) => ExecFeed) => {
      if (active) setState(current => {
        const old = current[source.sourceId] ?? emptyExecFeed(source), next = transform(old)
        return old === next ? current : { ...current, [source.sourceId]: next }
      })
    }
    for (const source of sources) {
      let socket: WebSocket | null = null, retry: number | undefined, attempts = 0, denied = false, loading = false
      const patch = (value: Partial<ExecFeed>) => update(source, feed => ({ ...feed, ...value }))
      const apply = (messages: unknown[]) => update(source, feed => {
        try { return messages.reduce<ExecFeed>((next, value) => applyExecMessage(next, value), feed) }
        catch (error) { return { ...feed, state: null, risk: null, error: error instanceof Error ? error.message : '执行数据校验失败' } }
      })
      const loadSnapshot = async () => {
        if (!active || denied || loading) return
        loading = true
        try {
          const response = await fetch(`${source.gatewayPrefix}/snapshot`, {
            cache: 'no-store', credentials: 'same-origin', signal: AbortSignal.any([controller.signal, AbortSignal.timeout(8_000)]),
          })
          if (!active) return
          serverClock.observe(response.headers.get('Date'))
          if (response.status === 401 || response.status === 403) {
            denied = true; window.clearTimeout(retry); socket?.close()
            patch({ connection: 'denied', state: null, risk: null, error: null }); return
          }
          if (!response.ok) throw new Error(`快照读取失败 (${response.status})`)
          const snapshot: unknown = await response.json()
          if (!snapshot || typeof snapshot !== 'object' || !('entries' in snapshot) || !Array.isArray(snapshot.entries)) throw new Error('执行快照格式无效')
          apply(snapshot.entries)
        } catch (error) {
          if (active) patch({ error: error instanceof Error ? error.message : '快照读取失败' })
        } finally { loading = false }
      }
      const connect = () => {
        if (!active || denied) return
        patch({ connection: 'connecting' })
        const url = new URL(`${source.gatewayPrefix}/ws`, location.origin)
        url.protocol = location.protocol === 'https:' ? 'wss:' : 'ws:'
        const reconnect = () => {
          if (!active || denied) return
          patch({ connection: 'disconnected' })
          retry = window.setTimeout(connect, Math.min(1_000 * 2 ** attempts++, 30_000))
        }
        try { socket = new WebSocket(url) } catch { reconnect(); return }
        socket.onopen = () => { if (active) { attempts = 0; patch({ connection: 'live', error: null }); void loadSnapshot() } }
        socket.onmessage = event => {
          if (!active || denied) return
          try { apply([JSON.parse(event.data)]) } catch { patch({ state: null, risk: null, error: '执行消息解析失败' }) }
        }
        socket.onerror = () => socket?.close()
        socket.onclose = reconnect
      }
      connect(); void loadSnapshot()
      const snapshotTimer = window.setInterval(() => void loadSnapshot(), 30_000)
      refreshers.push(() => { void loadSnapshot() })
      cleanups.push(() => { window.clearTimeout(retry); window.clearInterval(snapshotTimer); socket?.close() })
    }
    refreshRef.current = () => refreshers.forEach(refresh => refresh())
    const clockTimer = window.setInterval(() => setNow(serverClock.now()), 1_000)
    return () => { active = false; controller.abort(); window.clearInterval(clockTimer); cleanups.forEach(cleanup => cleanup()); refreshRef.current = () => {} }
  }, [sources])
  return { feeds: sources.map(s => state[s.sourceId] ?? emptyExecFeed(s)), now, refresh }
}
