const MINUTE_MS = 60_000

/** Keep the 30D shortcut inside the server's rolling candle retention. */
export function retainedQuickRangeStartMs(
  range: string | null,
  startMs: number,
  endMs: number,
  availableFromUs: number | null,
  executionWindowSecs: number,
) {
  if (
    range !== '30D' ||
    availableFromUs == null ||
    !Number.isFinite(availableFromUs) ||
    availableFromUs <= 0 ||
    !Number.isFinite(executionWindowSecs) ||
    executionWindowSecs <= 0
  ) return startMs

  const availableFromMs = Math.ceil(availableFromUs / 1_000)
  // A delta crossing the start still needs all five pricing minutes. The
  // server boundary already includes the preceding baseline candle; reserve
  // the execution window too so a slow backfill can survive subsequent polls.
  const nextStartMs = availableFromMs + Math.max(MINUTE_MS, executionWindowSecs * 1_000)
  return startMs < nextStartMs && nextStartMs < endMs ? nextStartMs : startMs
}
