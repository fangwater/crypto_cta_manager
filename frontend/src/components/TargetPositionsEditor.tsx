import { Minus, Plus, Search, Trash2, TrendingDown, TrendingUp } from 'lucide-react'
import { useMemo, useState } from 'react'
import { quantity } from '../format'
import { cn } from '../lib/cn'
import {
  ALLOWED_TARGET_SIGNALS,
  signalLabel,
  type TargetMap,
  type TargetPosition,
  type TargetSignal,
} from '../lib/targetPositions'
import { Badge } from './ui/Badge'
import { Button } from './ui/Button'
import { Input } from './ui/Field'

function normalizeSymbol(raw: string) {
  return raw.trim().toUpperCase()
}

function directionMeta(value: number) {
  if (value > 0) {
    return {
      label: '多',
      tone: 'success' as const,
      icon: TrendingUp,
      valueClass: 'text-emerald-700',
    }
  }
  if (value < 0) {
    return {
      label: '空',
      tone: 'warning' as const,
      icon: TrendingDown,
      valueClass: 'text-rose-700',
    }
  }
  return {
    label: '平',
    tone: 'neutral' as const,
    icon: Minus,
    valueClass: 'text-subtle',
  }
}

export function TargetPositionsEditor({
  targets,
  onChange,
}: {
  targets: TargetMap
  onChange: (targets: TargetMap) => void
}) {
  const [search, setSearch] = useState('')
  const [hideZero, setHideZero] = useState(true)
  const [newSymbol, setNewSymbol] = useState('')
  const [newQty, setNewQty] = useState('')
  const [newSignal, setNewSignal] = useState<TargetSignal>(0)
  const [draftSymbols, setDraftSymbols] = useState<Record<string, string>>({})

  const stats = useMemo(() => {
    const entries = Object.entries(targets)
    const quantities = entries.map(([, value]) => value.qty)
    return {
      total: entries.length,
      active: quantities.filter((value) => value !== 0).length,
      long: quantities.filter((value) => value > 0).length,
      short: quantities.filter((value) => value < 0).length,
      taker: entries.filter(([, value]) => Math.abs(value.signal) === 1).length,
    }
  }, [targets])

  const rows = useMemo(() => {
    const query = search.trim().toUpperCase()
    return Object.entries(targets)
      .map(([symbol, value]) => ({ symbol, value }))
      .filter(({ symbol, value }) => {
        if (hideZero && value.qty === 0) return false
        if (query && !symbol.toUpperCase().includes(query)) return false
        return true
      })
      .sort((left, right) => left.symbol.localeCompare(right.symbol))
  }, [hideZero, search, targets])

  function commitSymbolRename(oldSymbol: string) {
    const draft = draftSymbols[oldSymbol]
    if (draft === undefined) return
    const nextSymbol = normalizeSymbol(draft)
    setDraftSymbols((current) => {
      const next = { ...current }
      delete next[oldSymbol]
      return next
    })
    if (!nextSymbol || nextSymbol === oldSymbol) return
    if (nextSymbol in targets) return
    const next = { ...targets }
    next[nextSymbol] = next[oldSymbol] ?? { qty: 0, signal: 0 }
    delete next[oldSymbol]
    onChange(next)
  }

  function updateTarget(symbol: string, patch: Partial<TargetPosition>) {
    const current = targets[symbol] ?? { qty: 0, signal: 0 }
    onChange({ ...targets, [symbol]: { ...current, ...patch } })
  }

  function updateQuantity(symbol: string, raw: string) {
    if (raw.trim() === '' || raw.trim() === '-') {
      updateTarget(symbol, { qty: 0 })
      return
    }
    const value = Number(raw)
    if (!Number.isFinite(value)) return
    updateTarget(symbol, { qty: value })
  }

  function removeSymbol(symbol: string) {
    const next = { ...targets }
    delete next[symbol]
    onChange(next)
    setDraftSymbols((current) => {
      if (!(symbol in current)) return current
      const draft = { ...current }
      delete draft[symbol]
      return draft
    })
  }

  function addSymbol() {
    const symbol = normalizeSymbol(newSymbol)
    const value = Number(newQty)
    if (!symbol) return
    if (!Number.isFinite(value)) return
    if (symbol in targets) return
    onChange({ ...targets, [symbol]: { qty: value, signal: newSignal } })
    setNewSymbol('')
    setNewQty('')
    setNewSignal(0)
  }

  return (
    <div className="overflow-hidden rounded-xl border border-border-soft bg-canvas/30">
      <div className="flex flex-col gap-3 border-b border-border-soft bg-surface/80 px-4 py-3 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex flex-wrap items-center gap-2">
          <p className="text-sm font-medium text-ink">目标仓位</p>
          <Badge tone="neutral">{stats.total} 品种</Badge>
          <Badge tone="brand">{stats.active} 非零</Badge>
          {stats.long > 0 && <Badge tone="success">{stats.long} 多</Badge>}
          {stats.short > 0 && <Badge tone="warning">{stats.short} 空</Badge>}
          {stats.taker > 0 && <Badge tone="brand">{stats.taker} 本轮 taker</Badge>}
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <label className="inline-flex items-center gap-2 rounded-lg border border-border bg-surface px-3 py-1.5 text-xs text-muted">
            <input
              type="checkbox"
              className="accent-brand"
              checked={hideZero}
              onChange={(event) => setHideZero(event.target.checked)}
            />
            隐藏零仓位
          </label>
          <div className="relative min-w-[180px] flex-1 sm:flex-none">
            <Search size={14} className="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-subtle" />
            <Input
              value={search}
              onChange={(event) => setSearch(event.target.value)}
              placeholder="搜索品种"
              className="pl-9"
            />
          </div>
        </div>
      </div>

      {rows.length === 0 ? (
        <div className="px-4 py-12 text-center">
          <p className="text-sm font-medium text-ink">
            {stats.total === 0 ? '还没有目标仓位' : '没有匹配的品种'}
          </p>
          <p className="mt-1 text-xs text-muted">
            {stats.total === 0
              ? '在下方添加品种与目标数量。'
              : '调整筛选条件，或关闭「隐藏零仓位」。'}
          </p>
        </div>
      ) : (
        <div className="max-h-[420px] overflow-auto">
          <table className="w-full min-w-[640px] border-collapse text-sm">
            <thead className="sticky top-0 z-10 bg-canvas/95 backdrop-blur-sm">
              <tr className="border-b border-border-soft text-left text-[11px] uppercase tracking-[0.12em] text-subtle">
                <th className="px-4 py-2.5 font-medium">品种</th>
                <th className="px-3 py-2.5 font-medium">方向</th>
                <th className="px-3 py-2.5 font-medium">目标数量</th>
                <th className="px-3 py-2.5 font-medium">Signal</th>
                <th className="px-4 py-2.5 text-right font-medium">操作</th>
              </tr>
            </thead>
            <tbody>
              {rows.map(({ symbol, value }) => {
                const meta = directionMeta(value.qty)
                const DirectionIcon = meta.icon
                return (
                  <tr
                    key={symbol}
                    className="border-b border-border-soft/80 transition-colors hover:bg-surface/70"
                  >
                    <td className="px-4 py-2.5">
                      <Input
                        value={draftSymbols[symbol] ?? symbol}
                        onChange={(event) =>
                          setDraftSymbols((current) => ({
                            ...current,
                            [symbol]: event.target.value.toUpperCase(),
                          }))
                        }
                        onBlur={() => commitSymbolRename(symbol)}
                        className="h-9 font-mono text-[13px]"
                      />
                    </td>
                    <td className="px-3 py-2.5">
                      <span
                        className={cn(
                          'inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-[11px] font-medium',
                          value.qty > 0 && 'bg-emerald-50 text-emerald-700',
                          value.qty < 0 && 'bg-amber-50 text-amber-700',
                          value.qty === 0 && 'bg-slate-100 text-subtle',
                        )}
                      >
                        <DirectionIcon size={12} />
                        {meta.label}
                      </span>
                    </td>
                    <td className="px-3 py-2.5">
                      <Input
                        type="number"
                        step="any"
                        value={Number.isFinite(value.qty) ? value.qty : 0}
                        onChange={(event) => updateQuantity(symbol, event.target.value)}
                        className={cn('h-9 tabular-nums text-right font-mono text-[13px]', meta.valueClass)}
                      />
                    </td>
                    <td className="px-3 py-2.5">
                      <select
                        className="h-9 w-full rounded-lg border border-border bg-surface px-2 text-[13px] text-ink"
                        value={value.signal}
                        onChange={(event) =>
                          updateTarget(symbol, {
                            signal: Number(event.target.value) as TargetSignal,
                          })
                        }
                      >
                        {ALLOWED_TARGET_SIGNALS.map((signal) => (
                          <option key={signal} value={signal}>
                            {signal} · {signalLabel(signal)}
                          </option>
                        ))}
                      </select>
                    </td>
                    <td className="px-4 py-2.5 text-right">
                      <button
                        type="button"
                        className="inline-flex h-9 w-9 items-center justify-center rounded-lg border border-transparent text-muted transition-colors hover:border-rose-200 hover:bg-rose-50 hover:text-rose-700"
                        aria-label={`删除 ${symbol}`}
                        onClick={() => removeSymbol(symbol)}
                      >
                        <Trash2 size={15} />
                      </button>
                    </td>
                  </tr>
                )
              })}
            </tbody>
          </table>
        </div>
      )}

      <div className="border-t border-border-soft bg-surface/80 px-4 py-3">
        <div className="grid gap-2 sm:grid-cols-[minmax(0,1.2fr)_minmax(0,1fr)_minmax(0,1fr)_auto] sm:items-end">
          <label className="grid gap-1.5 text-xs font-medium text-muted">
            新增品种
            <Input
              value={newSymbol}
              onChange={(event) => setNewSymbol(event.target.value.toUpperCase())}
              placeholder="例如 BTCUSDT"
              className="font-mono"
            />
          </label>
          <label className="grid gap-1.5 text-xs font-medium text-muted">
            目标数量
            <Input
              type="number"
              step="any"
              value={newQty}
              onChange={(event) => setNewQty(event.target.value)}
              placeholder="正为多，负为空"
              className="tabular-nums text-right font-mono"
            />
          </label>
          <label className="grid gap-1.5 text-xs font-medium text-muted">
            Signal
            <select
              className="h-10 rounded-lg border border-border bg-surface px-3 text-sm text-ink"
              value={newSignal}
              onChange={(event) => setNewSignal(Number(event.target.value) as TargetSignal)}
            >
              {ALLOWED_TARGET_SIGNALS.map((signal) => (
                <option key={signal} value={signal}>
                  {signal} · {signalLabel(signal)}
                </option>
              ))}
            </select>
          </label>
          <Button
            type="button"
            variant="secondary"
            className="sm:mb-0"
            disabled={!normalizeSymbol(newSymbol) || !Number.isFinite(Number(newQty))}
            onClick={addSymbol}
          >
            <Plus size={15} />
            添加
          </Button>
        </div>
        {stats.total > 0 && (
          <p className="mt-2 text-[11px] text-subtle">
            当前共 {stats.total} 个品种，其中 {stats.active} 个非零仓位。
            {stats.active > 0 &&
              ` 预览：${quantity(Object.values(targets).reduce((sum, value) => sum + Math.abs(value.qty), 0))} 绝对量合计。`}
            {stats.taker > 0 && ' ±1 表示该品种本轮全部用 taker，不走 taker 转 maker。'}
          </p>
        )}
      </div>
    </div>
  )
}
