import { Plus } from 'lucide-react'
import { Button } from './ui/Button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from './ui/Card'
import { cn } from '../lib/cn'

export function StrategyPicker({
  title,
  emptyLabel,
  items,
  selectedName,
  onSelect,
  onCreate,
  renderMeta,
  allowCreate = true,
}: {
  title: string
  emptyLabel: string
  items: Array<{ strategy_name: string }>
  selectedName: string
  onSelect: (name: string) => void
  onCreate: () => void
  renderMeta: (name: string) => string
  allowCreate?: boolean
}) {
  return (
    <Card className="h-fit lg:sticky lg:top-24">
      <CardHeader className="flex flex-row items-start justify-between gap-3">
        <div>
          <CardTitle>{title}</CardTitle>
          <CardDescription>{items.length} 个已保存</CardDescription>
        </div>
        {allowCreate && (
          <Button type="button" size="sm" variant="primary" onClick={onCreate}>
            <Plus size={14} /> 新建
          </Button>
        )}
      </CardHeader>
      <CardContent className="space-y-2 pt-0">
        {items.length === 0 ? (
          <div className="rounded-xl border border-dashed border-border px-4 py-8 text-center text-sm text-muted">
            {emptyLabel}
          </div>
        ) : (
          items.map((item) => {
            const active = selectedName === item.strategy_name
            return (
              <button
                key={item.strategy_name}
                type="button"
                onClick={() => onSelect(item.strategy_name)}
                className={cn(
                  'w-full rounded-xl border px-3 py-3 text-left transition-all',
                  active
                    ? 'border-brand bg-brand-soft shadow-sm'
                    : 'border-border-soft bg-canvas/40 hover:border-border hover:bg-surface',
                )}
              >
                <p className="truncate text-sm font-medium text-ink">{item.strategy_name}</p>
                <p className="mt-1 text-xs text-muted">{renderMeta(item.strategy_name)}</p>
              </button>
            )
          })
        )}
      </CardContent>
    </Card>
  )
}
