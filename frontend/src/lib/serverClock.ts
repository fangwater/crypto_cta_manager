/** HTTP Date is rounded down to a second. Start elapsed time at receipt, so
 * network delay cannot move a "now" query beyond the server's current time.
 * A monotonic clock also survives changes to the browser's wall clock.
 */
export function createServerClock(
  elapsedMs = () => performance.now(),
  localNowMs = () => Date.now(),
) {
  let serverMs: number | null = null
  let receivedAtMs = 0
  return {
    observe(dateHeader: string | null) {
      if (!dateHeader) return
      const parsed = Date.parse(dateHeader)
      if (!Number.isFinite(parsed)) return
      serverMs = parsed
      receivedAtMs = elapsedMs()
    },
    now() {
      return serverMs === null
        ? localNowMs()
        : Math.floor(serverMs + Math.max(0, elapsedMs() - receivedAtMs))
    },
  }
}

export const serverClock = createServerClock()
