import { useEffect, useMemo, useState } from 'react'
import { Button } from './ui/Button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from './ui/Card'
import { FieldHint, Input } from './ui/Field'
import { cn } from '../lib/cn'
import { money } from '../format'
import type { AccountBinding } from '../types'

const BAR_COLORS = ['#2563eb', '#0d9488', '#d97706', '#7c3aed', '#db2777', '#059669']
const SUM_TOLERANCE = 0.01

function parsePercent(raw: string): number | null {
  const value = Number(raw)
  if (!Number.isFinite(value) || value <= 0) return null
  return value
}

function formatShares(value: number) {
  return String(Number(value.toFixed(4)))
}

export function AllocationEditor({
  bindings,
  boundEquity,
  saving,
  onSave,
}: {
  bindings: AccountBinding[]
  boundEquity: number
  saving?: boolean
  onSave: (allocations: Record<string, number>) => void | Promise<void>
}) {
  const [draft, setDraft] = useState<Record<string, string>>({})
  const [editing, setEditing] = useState<string | null>(null)
  const [localError, setLocalError] = useState<string | null>(null)

  const serverKey = bindings
    .map((binding) => `${binding.binding_name}:${binding.allocation_ratio}`)
    .join('|')

  useEffect(() => {
    const next: Record<string, string> = {}
    for (const binding of bindings) {
      next[binding.binding_name] = (binding.allocation_ratio * 100).toFixed(2)
    }
    setDraft(next)
    setEditing(null)
    setLocalError(null)
  }, [serverKey, bindings])

  const parsed = useMemo(() => {
    const values: Record<string, number | null> = {}
    let sum = 0
    let valid = true
    for (const binding of bindings) {
      const value = parsePercent(draft[binding.binding_name] ?? '')
      values[binding.binding_name] = value
      if (value === null) {
        valid = false
        continue
      }
      sum += value
    }
    const matches = valid && Math.abs(sum - 100) <= SUM_TOLERANCE
    return { values, sum, valid, matches }
  }, [bindings, draft])

  const dirty = bindings.some(
    (binding) =>
      Number(draft[binding.binding_name]) !== Number((binding.allocation_ratio * 100).toFixed(2)),
  )

  if (bindings.length === 0) return null

  function commitCell(name: string, raw: string) {
    const value = parsePercent(raw)
    setDraft((current) => ({
      ...current,
      [name]: value === null ? raw.trim() : value.toFixed(2),
    }))
    setEditing(null)
  }

  function handleSave() {
    if (!parsed.valid) {
      setLocalError('每条策略的占比必须是大于 0 的数字')
      return
    }
    if (!parsed.matches) {
      setLocalError(`合计为 ${parsed.sum.toFixed(2)}%，必须等于 100%`)
      return
    }
    const allocations: Record<string, number> = {}
    for (const binding of bindings) {
      allocations[binding.binding_name] = (parsed.values[binding.binding_name] ?? 0) / 100
    }
    setLocalError(null)
    void onSave(allocations)
  }

  const remainder = 100 - parsed.sum
  const remainderText = !parsed.valid
    ? '请把每条策略都填成大于 0 的百分比'
    : Math.abs(remainder) <= SUM_TOLERANCE
      ? '合计等于 100%，可以保存'
      : remainder > 0
        ? `还差 ${remainder.toFixed(2)}%，合计必须等于 100%`
        : `超出 ${Math.abs(remainder).toFixed(2)}%，合计必须等于 100%`

  return (
    <Card>
      <CardHeader>
        <CardTitle>策略占比</CardTitle>
        <CardDescription>
          双击某条策略的百分比直接改数字，改完后点一次「保存占比」。各策略互不挤占；保存时校验合计等于
          100%。这只改 Manager 本地份数，还需要再点各策略的「发布到 Exec」才会改仓位。
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="flex h-3 overflow-hidden rounded-full bg-canvas">
          {bindings.map((binding, index) => {
            const width = parsed.values[binding.binding_name]
            return (
              <div
                key={binding.binding_name}
                className="h-full"
                style={{
                  width: `${Math.max(width ?? 0, 0)}%`,
                  backgroundColor: BAR_COLORS[index % BAR_COLORS.length],
                }}
                title={`${binding.position_strategy_name} ${draft[binding.binding_name] ?? ''}%`}
              />
            )
          })}
        </div>
        <div className="overflow-hidden rounded-xl border border-border">
          <table className="w-full text-sm">
            <thead className="bg-canvas text-xs text-muted">
              <tr>
                <th className="px-3 py-2 text-left font-medium">策略</th>
                <th className="w-36 px-3 py-2 text-right font-medium">占比</th>
                <th className="w-28 px-3 py-2 text-right font-medium">折合份数</th>
                <th className="hidden px-3 py-2 text-right font-medium sm:table-cell">占用参考权益</th>
              </tr>
            </thead>
            <tbody>
              {bindings.map((binding, index) => {
                const percentValue = parsed.values[binding.binding_name]
                const shares =
                  percentValue === null || boundEquity <= 0
                    ? null
                    : ((percentValue / 100) * boundEquity) / binding.position_equity_usdt
                const occupied =
                  percentValue === null || boundEquity <= 0
                    ? null
                    : (percentValue / 100) * boundEquity
                const isEditing = editing === binding.binding_name
                return (
                  <tr key={binding.binding_name} className="border-t border-border-soft">
                    <td className="px-3 py-2">
                      <span className="flex items-center gap-2">
                        <span
                          className="h-2.5 w-2.5 shrink-0 rounded-full"
                          style={{ backgroundColor: BAR_COLORS[index % BAR_COLORS.length] }}
                        />
                        <span className="font-medium text-ink">{binding.position_strategy_name}</span>
                      </span>
                    </td>
                    <td className="px-3 py-2 text-right">
                      {isEditing ? (
                        <Input
                          autoFocus
                          className="ml-auto h-8 w-24 py-1 text-right"
                          inputMode="decimal"
                          value={draft[binding.binding_name] ?? ''}
                          onChange={(event) =>
                            setDraft((current) => ({
                              ...current,
                              [binding.binding_name]: event.target.value,
                            }))
                          }
                          onBlur={(event) => commitCell(binding.binding_name, event.target.value)}
                          onKeyDown={(event) => {
                            if (event.key === 'Enter') {
                              event.preventDefault()
                              commitCell(binding.binding_name, event.currentTarget.value)
                            }
                            if (event.key === 'Escape') {
                              event.preventDefault()
                              setDraft((current) => ({
                                ...current,
                                [binding.binding_name]: (
                                  binding.allocation_ratio * 100
                                ).toFixed(2),
                              }))
                              setEditing(null)
                            }
                          }}
                        />
                      ) : (
                        <button
                          type="button"
                          className="ml-auto block w-full rounded-md px-2 py-1 text-right font-semibold text-ink hover:bg-canvas"
                          title="双击修改"
                          onDoubleClick={() => setEditing(binding.binding_name)}
                        >
                          {(draft[binding.binding_name] ?? '0')}%
                        </button>
                      )}
                    </td>
                    <td className="px-3 py-2 text-right tabular-nums text-muted">
                      {shares === null ? '—' : formatShares(shares)}
                    </td>
                    <td className="hidden px-3 py-2 text-right tabular-nums text-muted sm:table-cell">
                      {occupied === null ? '—' : `${money(occupied)} USDT`}
                    </td>
                  </tr>
                )
              })}
            </tbody>
          </table>
        </div>
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div>
            <p
              className={cn(
                'text-sm font-semibold',
                parsed.matches ? 'text-ink' : 'text-danger',
              )}
            >
              合计 {parsed.valid ? `${parsed.sum.toFixed(2)}%` : '—'}
            </p>
            <FieldHint className={parsed.matches ? undefined : 'text-danger'}>
              {remainderText}
            </FieldHint>
            {localError ? <FieldHint className="text-danger">{localError}</FieldHint> : null}
          </div>
          <Button
            type="button"
            variant="primary"
            disabled={saving || !parsed.matches || !dirty}
            onClick={handleSave}
          >
            保存占比
          </Button>
        </div>
      </CardContent>
    </Card>
  )
}
