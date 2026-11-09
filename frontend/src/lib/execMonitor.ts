/** The existing native Exec Viz wire contract; source identity comes from its
 * authenticated gateway, never from a strategy name or a shared symbol. */
export interface ExecSource { sourceId: string; venue: string; gatewayPrefix: string }
export const EXEC_STALE_MS = 5_000
const amountKeys = ['target_qty', 'current_qty', 'effective_position_qty', 'delta_qty', 'live_order_qty', 'pending_qty',
  'account_position_qty', 'target_usdt', 'current_usdt', 'account_position_usdt', 'delta_usdt', 'live_order_usdt', 'pending_usdt',
  'active_batches', 'remaining_batches', 'estimated_completion_ts_ms', 'source_updated_at_ms'] as const
export type AmountKey = typeof amountKeys[number]
export type ExecRow = Record<AmountKey, number | null> & {
  key: string; sourceId: string; venue: string; symbol: string; strategy_name: string
  position_allocated: boolean; execution_complete: boolean; completion_reason: string
}
export interface ExecState { tsMs: number; ready: boolean; rows: ExecRow[] }
const riskKeys = ['equity_usdt', 'long_notional_usdt', 'short_notional_usdt', 'net_notional_usdt', 'gross_notional_usdt'] as const
export type RiskKey = typeof riskKeys[number]
export type ExecRisk = Record<RiskKey, number | null> & { tsMs: number }
export interface ExecFeed {
  source: ExecSource
  connection: 'connecting' | 'live' | 'disconnected' | 'denied'
  state: ExecState | null
  risk: ExecRisk | null
  error: string | null
}
export const emptyExecFeed = (source: ExecSource): ExecFeed => ({ source, connection: 'connecting', state: null, risk: null, error: null })
export const finiteAmount = (value: unknown): value is number => typeof value === 'number' && Number.isFinite(value)
function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('执行数据格式无效')
  return value as Record<string, unknown>
}
function amount(value: unknown): number | null {
  if (value == null) return null
  if (!finiteAmount(value)) throw new Error('执行数据包含无效数值')
  return value
}
function timestamp(value: unknown) {
  if (!finiteAmount(value) || !Number.isSafeInteger(value) || value <= 0) throw new Error('执行时间戳无效')
  return value
}
function market(source: ExecSource, symbol: string) {
  if (/^[A-Z0-9]+(?:USDT|USDC)$/.test(symbol)) return source.venue
  if (source.venue === 'okex-futures' && /^[A-Z0-9]+$/.test(symbol) && !symbol.endsWith('USD')) return 'okex-margin'
  throw new Error('执行品种不属于当前市场')
}
function parseRow(source: ExecSource, value: unknown): ExecRow {
  const raw = object(value)
  if (typeof raw.symbol !== 'string' || typeof raw.strategy_name !== 'string' || !raw.strategy_name) throw new Error('执行记录缺少品种或策略')
  const venue = market(source, raw.symbol)
  const amounts = Object.fromEntries(amountKeys.map(key => [key, amount(raw[key])])) as Record<AmountKey, number | null>
  return { ...amounts, key: JSON.stringify([source.sourceId, venue, raw.symbol, raw.strategy_name]), sourceId: source.sourceId,
    venue, symbol: raw.symbol, strategy_name: raw.strategy_name, position_allocated: raw.position_allocated === true,
    execution_complete: raw.execution_complete === true, completion_reason: typeof raw.completion_reason === 'string' ? raw.completion_reason : '' }
}

/** Ignore older HTTP snapshots arriving after newer WebSocket updates. Retain
 * the producer timestamp so rereading a cached snapshot cannot make it fresh. */
export function applyExecMessage(feed: ExecFeed, value: unknown): ExecFeed {
  const message = object(value)
  if (message.type !== 'exec_pre_trade_state' && message.type !== 'exec_pre_trade_risk') return feed
  if (message.venue !== feed.source.venue) throw new Error('执行数据交易所不匹配')
  const entry = object(message.entry), tsMs = timestamp(entry.ts_ms)
  if (message.type === 'exec_pre_trade_state') {
    if (feed.state && tsMs <= feed.state.tsMs) return feed
    if (!Array.isArray(entry.rows) || typeof entry.position_ready !== 'boolean') throw new Error('仓位快照格式无效')
    const rows = entry.rows.map(row => parseRow(feed.source, row))
    if (new Set(rows.map(r => r.key)).size !== rows.length) throw new Error('仓位快照包含重复记录')
    return { ...feed, error: null, state: { tsMs, ready: entry.position_ready, rows } }
  }
  if (feed.risk && tsMs <= feed.risk.tsMs) return feed
  if (entry.venue !== feed.source.venue) throw new Error('风险数据交易所不匹配')
  const amounts = Object.fromEntries(riskKeys.map(key => [key, amount(entry[key])])) as Record<RiskKey, number | null>
  for (const key of ['long_notional_usdt', 'short_notional_usdt', 'gross_notional_usdt'] as const) {
    if (amounts[key] != null && amounts[key] < 0) throw new Error('风险名义金额无效')
  }
  return { ...feed, error: null, risk: { tsMs, ...amounts } }
}

export function freshExecSample(sample: { tsMs: number } | null, now: number) {
  return sample != null && sample.tsMs <= now + 1_000 && now - sample.tsMs <= EXEC_STALE_MS
}
function usable(feed: ExecFeed, sample: { tsMs: number } | null, now: number) {
  return feed.connection === 'live' && !feed.error && freshExecSample(sample, now)
}
export function execFeedLabel(feed: ExecFeed, now: number) {
  if (feed.connection === 'denied') return '无查看权限'
  if (feed.error) return feed.error
  if (feed.connection === 'disconnected') return '连接中断'
  if (feed.connection === 'connecting') return '连接中'
  if (!feed.state) return '等待执行数据'
  if (!freshExecSample(feed.state, now)) return '数据延迟'
  return feed.state.ready ? '实时' : '等待仓位'
}
export function sumKnown(values: Array<number | null>, absolute = false): number | null {
  return values.every(finiteAmount) ? values.reduce<number>((sum, v) => sum + (absolute ? Math.abs(v!) : v!), 0) : null
}
export function combinedExecRisk(feeds: ExecFeed[], now: number) {
  const complete = feeds.length > 0 && feeds.every(f => usable(f, f.risk, now))
  const values = Object.fromEntries(riskKeys.map(key => [key, complete ? sumKnown(feeds.map(f => f.risk![key])) : null])) as Record<RiskKey, number | null>
  return { ...values, complete, leverage: values.equity_usdt != null && values.equity_usdt > 0 && values.gross_notional_usdt != null
    ? values.gross_notional_usdt / values.equity_usdt : null }
}

export type ExecStatus = { label: string; tone: 'neutral' | 'success' | 'warning' | 'brand'; detail?: string }
const completionReasons: Record<string, string> = {
  target_reached: '已达到目标仓位', target_tolerance: '剩余量已进入目标容差', exchange_minimum: '剩余量低于交易所最小下单量',
  borrow_unavailable: '借币不足，本次执行已结束', symbol_not_tradable: '品种不可交易，已停止下单',
}
function working(row: ExecRow) { return (row.active_batches ?? 0) > 0 || Math.abs(row.live_order_usdt ?? 0) > 0.01 }
export function executionRowStatus(row: ExecRow, feed: ExecFeed, now: number): ExecStatus {
  if (!usable(feed, feed.state, now)) return { label: '数据延迟', tone: 'warning' }
  if (!feed.state?.ready || !row.position_allocated) return { label: '等待仓位', tone: 'neutral' }
  if (working(row)) return { label: '执行中', tone: 'brand' }
  if (row.execution_complete) return { label: '执行完成', tone: 'success', detail: completionReasons[row.completion_reason] ?? '执行已结束' }
  if (row.delta_usdt == null) return { label: '数据不完整', tone: 'warning' }
  return Math.abs(row.delta_usdt) > 0.01 ? { label: '待执行', tone: 'warning' } : { label: '已完成', tone: 'success' }
}
function idle(row: ExecRow) {
  return ['target_qty', 'current_qty', 'effective_position_qty', 'live_order_qty', 'pending_qty', 'account_position_qty'].every(key => {
    const value = row[key as AmountKey]; return value != null && Math.abs(value) <= 1e-12
  }) && row.active_batches === 0 && row.remaining_batches === 0
}
export interface ExecDisplayRow { row: ExecRow; feed: ExecFeed; status: ExecStatus }
export interface ExecGroup {
  key: string; label: string; detail: string; venue: string | null; children: ExecDisplayRow[]; status: ExecStatus
  target: number | null; current: number | null; live: number | null; pending: number | null
  deltaUsdt: number | null; liveUsdt: number | null; pendingUsdt: number | null; batches: number | null; eta: number | null
}
export interface ExecFilters { strategy: string; symbol: string; sourceId: string; groupBy: 'symbol' | 'strategy' }
const ledgerKey = (r: ExecRow) => JSON.stringify([r.sourceId, r.venue, r.symbol])
function sumRows(rows: ExecDisplayRow[], key: AmountKey, absolute = false) { return sumKnown(rows.map(x => x.row[key]), absolute) }
function consistentAccountValue(rows: ExecDisplayRow[], key: AmountKey) {
  const values = rows.map(r => r.row[key]), first = values[0]
  return first != null && values.every(v => v != null && Math.abs(v - first) <= Math.max(1, Math.abs(first)) * 1e-10) ? first : null
}
function groupStatus(rows: ExecDisplayRow[]): ExecStatus {
  for (const label of ['数据延迟', '数据不完整', '等待仓位', '执行中', '待执行']) {
    const found = rows.find(r => r.status.label === label); if (found) return found.status
  }
  return { label: '已完成', tone: 'success' }
}
function makeGroup(key: string, children: ExecDisplayRow[], by: 'symbol' | 'strategy', accountPosition: boolean): ExecGroup {
  const first = children[0].row
  const sameLedger = children.every(x => ledgerKey(x.row) === ledgerKey(first))
  const qty = (key: AmountKey) => sameLedger ? sumRows(children, key) : null
  const current = sameLedger && accountPosition ? consistentAccountValue(children, 'account_position_qty') : qty('current_qty')
  const targetUsdt = sumRows(children, 'target_usdt'), actualUsdt = consistentAccountValue(children, 'account_position_usdt')
  const statusesReady = children.every(x => x.row.position_allocated && x.feed.state?.ready)
  const deltaUsdt = accountPosition ? targetUsdt != null && actualUsdt != null ? targetUsdt - actualUsdt : null : sumRows(children, 'delta_usdt', true)
  const etaValues = children.filter(x => !x.row.execution_complete).map(x => x.row.estimated_completion_ts_ms)
  const eta = etaValues.length && etaValues.every(v => v != null && v > 0) ? Math.max(...etaValues as number[]) : null
  return { key, label: by === 'symbol' ? first.symbol : first.strategy_name,
    detail: by === 'symbol' ? `${new Set(children.map(x => x.row.strategy_name)).size} 个策略` : `${new Set(children.map(x => x.row.symbol)).size} 个品种 · ${new Set(children.map(x => x.row.sourceId)).size} 个交易所`,
    venue: children.every(x => x.row.venue === first.venue) ? first.venue : null, children, status: groupStatus(children),
    target: qty('target_qty'), current: statusesReady ? current : null, live: qty('live_order_qty'), pending: statusesReady ? qty('pending_qty') : null,
    deltaUsdt: statusesReady ? deltaUsdt : null, liveUsdt: sumRows(children, 'live_order_usdt', true), pendingUsdt: statusesReady ? sumRows(children, 'pending_usdt', true) : null,
    batches: sumRows(children, 'remaining_batches'), eta }
}
export function executionView(feeds: ExecFeed[], filters: ExecFilters, now: number) {
  const selected = feeds.filter(f => !filters.sourceId || f.source.sourceId === filters.sourceId)
  const sourceComplete = selected.length > 0 && selected.every(f => usable(f, f.state, now) && f.state?.ready)
  const allRows: ExecDisplayRow[] = feeds.flatMap(feed => (feed.state?.rows ?? []).map(original => {
    const row = original.execution_complete ? { ...original, pending_qty: 0, pending_usdt: 0 } : original
    return { row, feed, status: executionRowStatus(row, feed, now) }
  }))
  const strategies = [...new Set(allRows.map(x => x.row.strategy_name))].sort()
  const rows = allRows.filter(x => (!filters.sourceId || x.row.sourceId === filters.sourceId) && (!filters.strategy || x.row.strategy_name === filters.strategy)
    && (!filters.symbol || x.row.symbol.includes(filters.symbol.trim().toUpperCase())) && !idle(x.row))
  const complete = sourceComplete && rows.every(x => x.row.position_allocated)
  const ledgers = new Map<string, ExecDisplayRow[]>(), grouped = new Map<string, ExecDisplayRow[]>()
  for (const row of rows) {
    const key = ledgerKey(row.row); ledgers.set(key, [...(ledgers.get(key) ?? []), row])
    const groupKey = filters.groupBy === 'symbol' ? key : row.row.strategy_name
    grouped.set(groupKey, [...(grouped.get(groupKey) ?? []), row])
  }
  const sourceOrder = new Map(feeds.map((feed, index) => [feed.source.sourceId, index]))
  const groups = [...grouped].map(([key, children]) => makeGroup(key, children, filters.groupBy, filters.groupBy === 'symbol' && !filters.strategy))
    .sort((a, b) => a.label.localeCompare(b.label) || (sourceOrder.get(a.children[0].row.sourceId)! - sourceOrder.get(b.children[0].row.sourceId)!))
  const perLedger = [...ledgers].map(([key, children]) => makeGroup(key, children, 'symbol', !filters.strategy))
  return { groups, rows, strategies, complete, summary: {
    deltaUsdt: complete ? sumKnown(perLedger.map(g => g.deltaUsdt), true) : null,
    liveUsdt: complete ? sumRows(rows, 'live_order_usdt', true) : null,
    pendingUsdt: complete ? sumRows(rows, 'pending_usdt', true) : null,
    batches: complete ? sumRows(rows, 'remaining_batches') : null,
  } }
}
